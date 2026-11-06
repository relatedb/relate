//! Native execution of a recipe's cells (ADR-0130).
//!
//! One `:memory:` database per recipe; cells in document order, each through
//! `relate::run_script_wire`, the runner the browser reaches through the
//! WASM `executeScript`, and each result rendered with
//! `relate_formatter::recording`, as the browser's `renderResult` does.
//! Like the page, a run stops at the first cell that goes other than its
//! recipe says ([`surprise`]).

use relate::{run_script_wire, Database};
use relate_formatter::recording::{self, Recording};
use serde_json::Value;

use crate::recipe::{Cell, Recipe};

/// One cell's envelopes, in statement order, and their recording.
#[derive(Debug, Clone, PartialEq)]
pub struct CellRun {
    pub id: String,
    pub envelopes: Vec<Value>,
    pub recording: Recording,
}

/// The cells a recipe ran, in order, and why it stopped early, if it did.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeRun {
    pub cells: Vec<CellRun>,
    pub stopped: Option<String>,
}

/// How a cell's run differs from what its recipe says, if it does: an error
/// where none is expected, or success where `expect=error`.
pub fn surprise(cell: &Cell, envelopes: &[Value]) -> Option<String> {
    let error = envelopes.last().and_then(|e| e["error"].as_str());
    match (cell.expect_error, error) {
        (false, Some(msg)) => Some(format!("cell `{}` failed: {msg}", cell.id)),
        (true, None) => Some(format!(
            "cell `{}` is marked expect=error but succeeded",
            cell.id
        )),
        _ => None,
    }
}

/// Run `recipe`'s cells on a big-stack thread.
pub fn run_recipe(recipe: &Recipe) -> Result<RecipeRun, String> {
    let recipe = recipe.clone();
    relate_test_support::run_with_big_stack(move || run_on_this_thread(&recipe))
}

/// The one engine pair these recipes run. A second pair adds an arm in
/// [`open_for_pair`].
const ENGINE_PAIR: (&str, &str) = ("in-memory", "local");

fn open_for_pair(pair: (&str, &str)) -> Result<Database, String> {
    if pair == ("in-memory", "local") {
        Database::open(":memory:").map_err(|e| format!("open :memory:: {e}"))
    } else {
        Err(format!("no runner for engine pair {pair:?}"))
    }
}

fn run_on_this_thread(recipe: &Recipe) -> Result<RecipeRun, String> {
    let db = open_for_pair(ENGINE_PAIR)?;
    let mut conn = db.connect().map_err(|e| format!("connect: {e}"))?;
    let mut cells = Vec::new();
    for cell in &recipe.cells {
        let params = match &cell.params {
            Some(name) => Some(
                recipe
                    .data_block(name)
                    .ok_or_else(|| format!("cell `{}`: no `json data={name}` block", cell.id))?
                    .text
                    .as_str(),
            ),
            None => None,
        };
        let envelopes = run_script_wire(&mut conn, &cell.code, params);
        let recording =
            recording::render(&envelopes).map_err(|e| format!("cell `{}`: {e}", cell.id))?;
        let stopped = surprise(cell, &envelopes);
        cells.push(CellRun {
            id: cell.id.clone(),
            envelopes,
            recording,
        });
        if stopped.is_some() {
            return Ok(RecipeRun { cells, stopped });
        }
    }
    Ok(RecipeRun {
        cells,
        stopped: None,
    })
}

/// Problems with a run that do not depend on recorded output: a cell that
/// went other than its recipe says (and the cells it kept from running), or
/// an oversized result.
pub fn check_run(recipe: &Recipe, run: &RecipeRun) -> Vec<String> {
    let mut errs: Vec<String> = run.stopped.iter().cloned().collect();
    let skipped = recipe.cells.len() - run.cells.len();
    if skipped > 0 {
        errs.push(format!("the {skipped} cell(s) after it did not run"));
    }
    for cell in &run.cells {
        for env in &cell.envelopes {
            let n = env["rows"].as_array().map_or(0, Vec::len);
            if n > crate::MAX_RESULT_ROWS {
                errs.push(format!(
                    "cell `{}` returns {n} rows; add a LIMIT (max {})",
                    cell.id,
                    crate::MAX_RESULT_ROWS
                ));
            }
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surprise_agrees_with_the_javascript_policy() {
        // scripts/cookbook/lib/cells.mjs is tested with the same cases.
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../scripts/cookbook/lib/cells.cases.json"
        ))
        .unwrap();
        for case in cases {
            let cell = Cell {
                id: "a".into(),
                code: String::new(),
                setup: false,
                params: None,
                expect_error: case["expectError"].as_bool().unwrap(),
                line: 1,
                end: 0,
                result: None,
            };
            // `last: null` is a cell that ran no statement.
            let envelopes: Vec<Value> = match &case["last"] {
                Value::Null => Vec::new(),
                last => vec![last.clone()],
            };
            assert_eq!(
                surprise(&cell, &envelopes).as_deref(),
                case["problem"].as_str(),
                "{case}"
            );
        }
    }
}
