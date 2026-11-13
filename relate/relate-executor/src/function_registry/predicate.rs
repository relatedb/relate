//! `Predicate` function rows: the list-comprehension quantifiers,
//! `allReduce` and the boolean tests.

use super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

pub(super) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta {
        name: "all",
        category: FunctionCategory::Predicate,
        description: "Returns true if the predicate holds for all elements in the given `LIST<ANY>`.",
        special_signature: Some("(variable :: VARIABLE IN list :: LIST<ANY> WHERE predicate :: ANY) :: BOOLEAN"),
        args: &[
            ProcArgMeta::described(
                "variable",
                "A variable that can be used within the `WHERE` clause.",
                "ANY",
            ),
            ProcArgMeta::described(
                "list",
                "A predicate must hold for all elements in this list for the function to return `true`.",
                "LIST<ANY>",
            ),
        ],
        return_type: "BOOLEAN",
        deprecated_by: None,
    },
    FunctionMeta {
        name: "any",
        category: FunctionCategory::Predicate,
        description: "Returns true if the predicate holds for at least one element in the given `LIST<ANY>`.",
        special_signature: Some("(variable :: VARIABLE IN list :: LIST<ANY> WHERE predicate :: ANY) :: BOOLEAN"),
        args: &[
            ProcArgMeta::described(
                "variable",
                "A variable that can be used within the `WHERE` clause.",
                "ANY",
            ),
            ProcArgMeta::described(
                "list",
                "A list for the predicate to check against.",
                "LIST<ANY>",
            ),
        ],
        return_type: "BOOLEAN",
        deprecated_by: None,
    },
    FunctionMeta {
        name: "none",
        category: FunctionCategory::Predicate,
        description: "Returns true if the predicate holds for no element in the given `LIST<ANY>`.",
        special_signature: Some("(variable :: VARIABLE IN list :: LIST<ANY> WHERE predicate :: ANY) :: BOOLEAN"),
        args: &[
            ProcArgMeta::described(
                "variable",
                "A variable that can be used within the `WHERE` clause.",
                "ANY",
            ),
            ProcArgMeta::described(
                "list",
                "A list for the predicate to check against.",
                "LIST<ANY>",
            ),
        ],
        return_type: "BOOLEAN",
        deprecated_by: None,
    },
    FunctionMeta {
        name: "single",
        category: FunctionCategory::Predicate,
        description: "Returns true if the predicate holds for exactly one of the elements in the given `LIST<ANY>`.",
        special_signature: Some("(variable :: VARIABLE IN list :: LIST<ANY> WHERE predicate :: ANY) :: BOOLEAN"),
        args: &[
            ProcArgMeta::described(
                "variable",
                "A variable that can be used within the `WHERE` clause.",
                "ANY",
            ),
            ProcArgMeta::described(
                "list",
                "A list for the predicate to check against.",
                "LIST<ANY>",
            ),
        ],
        return_type: "BOOLEAN",
        deprecated_by: None,
    },
    FunctionMeta::described(
        "exists",
        FunctionCategory::Predicate,
        "Returns true if a match for the pattern exists in the graph.",
        &[
            ProcArgMeta::described("input", "A pattern to verify the existence of.", "ANY"),
        ],
        "BOOLEAN",
    ),
    FunctionMeta::described(
        "isEmpty",
        FunctionCategory::Predicate,
        "Checks whether a `STRING`, `MAP` or `LIST<ANY>` is empty.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be checked for emptiness.",
                "STRING | MAP | LIST<ANY>",
            ),
        ],
        "BOOLEAN",
    ),
    FunctionMeta::described(
        "property_exists",
        FunctionCategory::Predicate,
        "Returns true if the given node or relationship has a property with the given key, otherwise false. Returns null if the first argument is null.",
        &[
            ProcArgMeta::described(
                "element",
                "A node or relationship to check for the property.",
                "NODE | RELATIONSHIP",
            ),
            ProcArgMeta::described(
                "propertyKeyName",
                "The name of the property to check for.",
                "STRING",
            ),
        ],
        "BOOLEAN",
    ),
    FunctionMeta {
        name: "allReduce",
        category: FunctionCategory::Predicate,
        description: "Evaluates `expression` against each element of `LIST<ANY>`, with the result stored in `accumulator`. Returns true if `predicate` holds for each iteration of `accumulator`.",
        special_signature: Some("(accumulator :: VARIABLE = initial :: ANY, variable :: VARIABLE IN list :: LIST<ANY> | reducer :: ANY, predicate :: BOOLEAN) :: BOOLEAN"),
        args: &[
            ProcArgMeta::described(
                "accumulator = initial",
                "The variable holding the result of reducer for each iteration, set to the value initial.",
                "ANY",
            ),
            ProcArgMeta::described(
                "variable IN list | reducer",
                "The iteration over a list that binds each list element to var and evaluates reducer to update the accumulator.",
                "ANY",
            ),
            ProcArgMeta::described(
                "predicate",
                "The predicate applied to the accumulator value for each element in the list.",
                "BOOLEAN",
            ),
        ],
        return_type: "BOOLEAN",
        deprecated_by: None,
    },
];
