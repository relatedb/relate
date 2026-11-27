//! Timing, memory and the report of the shape matrix.
//!
//! Each graph group builds its store once; the cells of the group then run
//! in `rounds` interleaved rounds (round-major, so machine drift lands on
//! every cell alike). A cell reports the median of its rounds with the
//! 10th-90th percentile spread as its noise estimate, and the heap high
//! water the statement added (the counting allocator's peak over the
//! live bytes before the run — per cell, unlike the process peak RSS,
//! which only grows and is reported once per group).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use relate::{Connection, Value};
use relate_test_support::percentile;
use relate_test_support::sampling::process_peak_rss_bytes;
use serde_json::{json, Value as Json};

use super::graph::{self, GraphKey};
use super::Cell;
use crate::alloc;

/// A run's knobs.
pub struct Options {
    pub persons: usize,
    pub rounds: usize,
    /// The path visits one statement may cost: a cell's driver rows are
    /// cut to `budget / visits per start` (the starts follow), and a cell
    /// that cannot keep [`MIN_ROWS`] rows is reported `skipped`.
    pub budget: f64,
    pub label: String,
}

/// The fewest driver rows a cell keeps when the budget cuts them.
pub const MIN_ROWS: usize = 4;

/// One measured (or skipped) cell.
pub struct CellResult {
    pub cell: Cell,
    /// The driver rows the cell ran with (cut by the budget).
    pub persons: usize,
    pub starts_distinct: usize,
    pub est_visits: f64,
    /// `None`: ran. `Some(reason)`: skipped.
    pub skipped: Option<String>,
    /// The answer: `count(*)` for a count cell, the row count otherwise.
    pub answer: Option<i64>,
    pub times_ms: Vec<f64>,
    pub heap_peak_bytes: Vec<f64>,
}

/// Percentile summary of one cell's rounds.
pub struct Summary {
    pub median_ms: f64,
    pub p10_ms: f64,
    pub p90_ms: f64,
    pub min_ms: f64,
    /// `(p90 - p10) / median` in percent: the noise estimate.
    pub spread_pct: f64,
    pub heap_peak_mb: f64,
}

impl CellResult {
    /// Why the cell has no measurement, or `None` when it ran: the one
    /// reading both renderings share.
    pub fn skip_reason(&self) -> Option<String> {
        match (&self.skipped, self.times_ms.is_empty()) {
            (Some(reason), _) => Some(reason.clone()),
            (None, true) => Some("no rounds were run".to_string()),
            (None, false) => None,
        }
    }

    pub fn summary(&self) -> Option<Summary> {
        if self.times_ms.is_empty() {
            return None;
        }
        let median = percentile(&self.times_ms, 0.5);
        let (p10, p90) = (
            percentile(&self.times_ms, 0.1),
            percentile(&self.times_ms, 0.9),
        );
        Some(Summary {
            median_ms: median,
            p10_ms: p10,
            p90_ms: p90,
            min_ms: percentile(&self.times_ms, 0.0),
            spread_pct: if median > 0.0 {
                (p90 - p10) / median * 100.0
            } else {
                0.0
            },
            heap_peak_mb: percentile(&self.heap_peak_bytes, 0.5) / (1024.0 * 1024.0),
        })
    }
}

/// The measured report.
pub struct Report {
    pub label: String,
    pub persons: usize,
    pub rounds: usize,
    pub results: Vec<CellResult>,
    /// `(group name, process peak RSS bytes after the group)`.
    pub group_rss: Vec<(String, Option<u64>)>,
}

/// Run the statement once: the elapsed milliseconds, the heap peak it
/// added and the answer.
fn run_once(conn: &mut Connection, cell: &Cell) -> (f64, f64, i64) {
    let statement = cell.query();
    alloc::reset_peak();
    let base = alloc::live();
    let start = Instant::now();
    let result = conn
        .query_with_params(&statement, BTreeMap::new())
        .unwrap_or_else(|e| panic!("cell {}: `{statement}`: {e}", cell.id()));
    let elapsed = start.elapsed().as_secs_f64() * 1e3;
    let peak = (alloc::peak_live() - base).max(0) as f64;
    let answer = match cell.consumer {
        super::Consumer::Count => match result.rows.first().and_then(|r| r.first()) {
            Some(Value::Int(n)) => *n,
            other => panic!("cell {}: count answered {other:?}", cell.id()),
        },
        super::Consumer::Full | super::Consumer::Limit => result.rows.len() as i64,
    };
    std::hint::black_box(&result);
    (elapsed, peak, answer)
}

/// Run `cells` under `options`, grouped by the graph they share.
pub fn run(cells: &[Cell], options: &Options) -> Report {
    // The driver rows a cell may keep within the budget.
    let rows_for = |cell: &Cell| -> usize {
        ((options.budget / cell.visits_per_start()) as usize).min(options.persons)
    };
    let mut groups: BTreeMap<GraphKey, Vec<Cell>> = BTreeMap::new();
    let mut skipped = Vec::new();
    for cell in cells {
        let rows = rows_for(cell);
        if rows >= MIN_ROWS {
            groups
                .entry(GraphKey::of(cell, rows))
                .or_default()
                .push(*cell);
        } else {
            skipped.push(*cell);
        }
    }
    let mut report = Report {
        label: options.label.clone(),
        persons: options.persons,
        rounds: options.rounds,
        results: Vec::new(),
        group_rss: Vec::new(),
    };
    for cell in skipped {
        report.results.push(CellResult {
            cell,
            persons: 0,
            starts_distinct: 0,
            est_visits: cell.visits_per_start(),
            skipped: Some(format!(
                "{:.0} path visits per start leave under {MIN_ROWS} rows in the budget {:.0}",
                cell.visits_per_start(),
                options.budget
            )),
            answer: None,
            times_ms: Vec::new(),
            heap_peak_bytes: Vec::new(),
        });
    }
    for (key, members) in groups {
        let mut runnable: Vec<CellResult> = members
            .into_iter()
            .map(|cell| CellResult {
                cell,
                persons: key.persons,
                starts_distinct: cell.starts.distinct(key.persons),
                est_visits: cell.visits_per_start() * key.persons as f64,
                skipped: None,
                answer: None,
                times_ms: Vec::new(),
                heap_peak_bytes: Vec::new(),
            })
            .collect();
        eprintln!("group {}: building, {} cell(s)", key.name(), runnable.len());
        let db = graph::build(key);
        let mut conn = db.connect().expect("connect measuring session");
        for r in &mut runnable {
            // The untimed warm-up: caches, plan, and the answer check.
            let (_, _, answer) = run_once(&mut conn, &r.cell);
            assert!(answer > 0, "cell {} answered nothing", r.cell.id());
            r.answer = Some(answer);
        }
        for _ in 0..options.rounds {
            for r in &mut runnable {
                let (ms, peak, answer) = run_once(&mut conn, &r.cell);
                assert_eq!(
                    Some(answer),
                    r.answer,
                    "cell {} answered differently between runs",
                    r.cell.id()
                );
                r.times_ms.push(ms);
                r.heap_peak_bytes.push(peak);
            }
        }
        report
            .group_rss
            .push((key.name(), process_peak_rss_bytes()));
        report.results.extend(runnable);
    }
    report.results.sort_by_key(|r| r.cell.id());
    report
}

impl Report {
    pub fn to_json(&self) -> Json {
        let cells: Vec<Json> = self
            .results
            .iter()
            .map(|r| {
                let c = &r.cell;
                let mut cell = json!({
                    "id": c.id(),
                    "topology": c.topology.name(),
                    "fanout": c.fanout,
                    "starts": c.starts.name(),
                    "persons": r.persons,
                    "starts_distinct": r.starts_distinct,
                    "hops": c.hops.name(),
                    "typing": c.typing.name(),
                    "direction": c.direction.name(),
                    "consumer": c.consumer.name(),
                    "est_visits": r.est_visits,
                    "query": c.query(),
                });
                match r.skip_reason() {
                    Some(reason) => cell["skipped"] = json!(reason),
                    None => {
                        let s = r.summary().expect("a cell with rounds has a summary");
                        cell["answer"] = json!(r.answer);
                        cell["median_ms"] = json!(s.median_ms);
                        cell["p10_ms"] = json!(s.p10_ms);
                        cell["p90_ms"] = json!(s.p90_ms);
                        cell["min_ms"] = json!(s.min_ms);
                        cell["spread_pct"] = json!(s.spread_pct);
                        cell["heap_peak_mb"] = json!(s.heap_peak_mb);
                        cell["times_ms"] = json!(r.times_ms);
                    }
                }
                cell
            })
            .collect();
        let rss: Vec<Json> = self
            .group_rss
            .iter()
            .map(|(group, rss)| json!({"group": group, "process_peak_rss_bytes": rss}))
            .collect();
        json!({
            "label": self.label,
            "persons": self.persons,
            "rounds": self.rounds,
            "cells": cells,
            "groups": rss,
        })
    }

    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# Shape matrix: {}\n\n{} driver rows, {} interleaved rounds per cell; \
             spread = (p90 - p10) / median; heap = the statement's allocator high water.\n\n\
             | cell | rows | starts | median ms | spread % | min ms | heap MB | answer |\n\
             | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
            self.label, self.persons, self.rounds
        );
        for r in &self.results {
            match r.skip_reason() {
                Some(reason) => out.push_str(&format!(
                    "| {} | | | skipped: {reason} | | | | |\n",
                    r.cell.id()
                )),
                None => {
                    let s = r.summary().expect("a cell with rounds has a summary");
                    let answer = r.answer.map_or_else(|| "-".to_string(), |a| a.to_string());
                    out.push_str(&format!(
                        "| {} | {} | {} | {:.3} | {:.1} | {:.3} | {:.2} | {answer} |\n",
                        r.cell.id(),
                        r.persons,
                        r.starts_distinct,
                        s.median_ms,
                        s.spread_pct,
                        s.min_ms,
                        s.heap_peak_mb,
                    ));
                }
            }
        }
        out.push_str("\nProcess peak RSS after each graph group:\n\n");
        for (group, rss) in &self.group_rss {
            match rss {
                Some(bytes) => {
                    out.push_str(&format!("- {group}: {:.0} MB\n", *bytes as f64 / 1048576.0))
                }
                None => out.push_str(&format!("- {group}: unavailable\n")),
            }
        }
        out
    }
}

/// The relative change under which two medians are one result, on top of
/// each side's own measured noise: the memo's acceptance bar (about 5%).
pub const NOISE_FLOOR_PCT: f64 = 5.0;

/// Compare two report JSONs cell by cell: `after` against `before`.
pub fn compare(before: &Path, after: &Path) -> Result<String, String> {
    let load = |path: &Path| -> Result<BTreeMap<String, Json>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let json: Json =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let cells = json["cells"]
            .as_array()
            .ok_or_else(|| format!("{}: no `cells` array", path.display()))?;
        cells
            .iter()
            .map(|c| {
                let id = c["id"]
                    .as_str()
                    .ok_or_else(|| format!("{}: a cell has no string `id`", path.display()))?;
                Ok((id.to_string(), c.clone()))
            })
            .collect()
    };
    let (a, b) = (load(before)?, load(after)?);
    let mut out = String::from(
        "| cell | before ms | after ms | ratio | heap ratio | verdict |\n\
         | --- | ---: | ---: | ---: | ---: | --- |\n",
    );
    let mut regressions = 0;
    let row = |out: &mut String, id: &str, verdict: &str| {
        out.push_str(&format!("| {id} | | | | | {verdict} |\n"));
    };
    for id in a.keys().chain(b.keys().filter(|id| !a.contains_key(*id))) {
        let (Some(x), Some(y)) = (a.get(id), b.get(id)) else {
            regressions += 1;
            row(&mut out, id, "MISSING on one side");
            continue;
        };
        let (Some(t0), Some(t1)) = (x["median_ms"].as_f64(), y["median_ms"].as_f64()) else {
            regressions += 1;
            row(&mut out, id, "SKIPPED or unmeasured on one side");
            continue;
        };
        let spread = |c: &Json| {
            c["spread_pct"]
                .as_f64()
                .ok_or_else(|| format!("cell {id} has a median but no `spread_pct`"))
        };
        let noise = spread(x)?.max(spread(y)?) / 2.0;
        let band = noise.max(NOISE_FLOOR_PCT);
        let change = (t1 / t0 - 1.0) * 100.0;
        let verdict = if x["answer"] != y["answer"] {
            regressions += 1;
            "ANSWER DIFFERS".to_string()
        } else if change > band {
            regressions += 1;
            format!("SLOWER (> {band:.1}%)")
        } else if -change > band {
            format!("faster (> {band:.1}%)")
        } else {
            "same".to_string()
        };
        let heap = match (x["heap_peak_mb"].as_f64(), y["heap_peak_mb"].as_f64()) {
            (Some(h0), Some(h1)) if h0 > 0.0 => format!("{:.2}", h1 / h0),
            _ => "-".to_string(),
        };
        out.push_str(&format!(
            "| {id} | {t0:.3} | {t1:.3} | {:.2} | {heap} | {verdict} |\n",
            t1 / t0
        ));
    }
    out.push_str(&format!(
        "\n{regressions} regression(s), answer change(s) or unmatched cell(s).\n"
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(times: &[f64]) -> CellResult {
        CellResult {
            cell: Cell::BASELINE,
            persons: 100,
            starts_distinct: 10,
            est_visits: 1.0,
            skipped: None,
            answer: Some(1),
            times_ms: times.to_vec(),
            heap_peak_bytes: vec![1048576.0; times.len()],
        }
    }

    #[test]
    fn the_summary_reports_the_median_and_the_spread() {
        let s = result(&[10.0, 11.0, 12.0, 13.0, 14.0]).summary().unwrap();
        assert_eq!(s.median_ms, 12.0);
        assert_eq!(s.min_ms, 10.0);
        assert!(s.spread_pct > 0.0);
        assert_eq!(s.heap_peak_mb, 1.0);
        assert!(result(&[]).summary().is_none());
    }

    #[test]
    fn compare_flags_a_slowdown_beyond_noise_and_an_answer_change() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, median: f64, answer: i64| {
            let path = dir.path().join(name);
            let cell = json!({
                "id": "c", "median_ms": median, "spread_pct": 2.0,
                "heap_peak_mb": 1.0, "answer": answer
            });
            std::fs::write(&path, json!({"cells": [cell]}).to_string()).unwrap();
            path
        };
        let base = write("a.json", 10.0, 5);
        let slow = compare(&base, &write("b.json", 11.0, 5)).unwrap();
        assert!(slow.contains("SLOWER"), "{slow}");
        let same = compare(&base, &write("c.json", 10.2, 5)).unwrap();
        assert!(same.contains("same"), "{same}");
        let wrong = compare(&base, &write("d.json", 10.0, 6)).unwrap();
        assert!(wrong.contains("ANSWER DIFFERS"), "{wrong}");
    }
}
