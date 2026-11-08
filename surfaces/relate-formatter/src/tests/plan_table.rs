//! The plan-table presentation's own pins (LABS-449): the descriptor
//! list's drift-proof properties (headers and cells walk one list, the
//! extremes pinned by concrete names) and the moved model-side pins —
//! the never-executed derivation, the ratio cell, the footer line.

use super::super::plan_table::{
    never_executed, plan_columns, plan_row_cells, ratio_cell, warnings_footer_line, MetricKinds,
    PlanColumnFlags, PlanRow,
};
use relate_model::wire::{PlanMetricsWire, PlanPhasesWire, PlanProfiledFacts, PlanReadsWire};

/// One hand-built row facts bundle: a profiled node's metrics and
/// total, an estimate, and no warnings.
fn facts() -> PlanRow {
    PlanRow {
        operator: "+ ProduceResults".to_string(),
        id: 0,
        details: "1 item(s)".to_string(),
        estimated: Some(25),
        metrics: Some(PlanProfiledFacts {
            metrics: PlanMetricsWire {
                rows: 1,
                time_ns: 1_000,
                db_hits: 51,
                memory_bytes: 40,
                expression_built_bytes: 0,
                reads: PlanReadsWire {
                    hops: 51,
                    neighbor_walks: 3,
                    node_reads: 2,
                },
                phases: PlanPhasesWire {
                    seek_ns: 1,
                    walk_ns: 2,
                },
                start: None,
                kernel: None,
                batched: false,
                zone_skips: 0,
                frontier: Default::default(),
                frontier_decline: None,
            },
            total_time_ns: 2_000,
        }),
        input_rows: Some(1),
        is_root: true,
        warnings: vec![],
    }
}

/// The descriptor list's drift-proof properties (LABS-449): for every
/// flag bundle the header assembly and the cell assembly walk the one
/// filtered list — counts and orders match — and the two extremes are
/// pinned against concrete names, so a variant missing from the
/// hand-maintained render list (or a reordered one) fails loudly
/// instead of silently never rendering.
#[test]
fn header_and_cells_walk_the_same_descriptor_list() {
    // The all-off bundle renders only the base columns — pinned by
    // name, not by derivation.
    assert_eq!(
        plan_columns(PlanColumnFlags::default()),
        ["Operator", "Id", "Details"]
    );
    // The widest bundle renders every column, in the render order —
    // the concrete fifteen names, so the render list omitting or
    // misplacing a variant fails here.
    let all = plan_columns(PlanColumnFlags {
        estimate: true,
        metrics: Some(MetricKinds {
            nodes: true,
            rels: true,
            degree: true,
        }),
        warnings: true,
        executed: true,
    });
    assert_eq!(
        all,
        [
            "Operator",
            "Id",
            "Details",
            "Executed",
            "Estimated Rows",
            "Rows",
            "Time (ms)",
            "Total (ms)",
            "Db Hits",
            "Nodes",
            "Rels",
            "Degree",
            "Memory (Bytes)",
            "Expression Built (Bytes)",
            "Est/Actual",
            "Warnings",
        ]
    );
    // And for a spread of bundles, the cells walk the same filtered
    // list — the counts match by construction, asserted anyway.
    let bundles = [
        PlanColumnFlags {
            estimate: true,
            ..Default::default()
        },
        PlanColumnFlags {
            metrics: Some(MetricKinds::default()),
            ..Default::default()
        },
        PlanColumnFlags {
            estimate: true,
            metrics: Some(MetricKinds::default()),
            executed: true,
            warnings: true,
        },
        PlanColumnFlags {
            metrics: Some(MetricKinds {
                nodes: true,
                rels: true,
                degree: true,
            }),
            ..Default::default()
        },
    ];
    for flags in bundles {
        let headers = plan_columns(flags);
        let cells = plan_row_cells(&facts(), flags);
        assert_eq!(
            headers.len(),
            cells.len(),
            "the header and the cells walk one list: {headers:?}"
        );
    }
}
/// The never-executed derivation policy: zero rows in and zero rows out —
/// the input fact arrives as the input clause's produced rows, `None`
/// when the operator starts from the statement's seed row.
#[test]
fn never_executed_derives_from_zero_rows_in_and_out() {
    assert!(
        never_executed(0, Some(0), false),
        "starved: zero in, zero out"
    );
    assert!(
        !never_executed(0, Some(1), false),
        "an input row means work"
    );
    assert!(
        !never_executed(0, None, false),
        "no input clause: the seed row"
    );
    assert!(
        !never_executed(1, Some(0), false),
        "producing rows from nothing is execution"
    );
    assert!(
        !never_executed(0, Some(0), true),
        "the root always executes: its zeros are the answer"
    );
}

/// The estimated-vs-actual ratio cell policy: the ratio renders `{n.n}x`,
/// the warning marker rides it at a 10x overshoot, and an undefined
/// ratio (no actual rows) renders nothing rather than a fabricated
/// number.
#[test]
fn ratio_cell_flags_a_ten_x_overshoot() {
    assert_eq!(ratio_cell(25, 1).as_deref(), Some("25.0x !"));
    assert_eq!(
        ratio_cell(10, 1).as_deref(),
        Some("10.0x !"),
        "10x or greater"
    );
    assert_eq!(
        ratio_cell(9, 1).as_deref(),
        Some("9.0x"),
        "under the marker"
    );
    assert_eq!(
        ratio_cell(25, 25).as_deref(),
        Some("1.0x"),
        "matching estimate"
    );
    assert_eq!(ratio_cell(25, 0), None, "undefined: no actual rows");
}

/// The one footer-line policy: one `Id <id>: <message>` entry per warning,
/// `; `-joined after the `Warnings` header.
#[test]
fn warnings_footer_line_joins_ids_and_messages() {
    assert_eq!(
        warnings_footer_line(&[(2, "the MATCH patterns share no join anchor")]),
        "Warnings: Id 2: the MATCH patterns share no join anchor"
    );
    assert_eq!(
        warnings_footer_line(&[
            (1, "estimated rows 10x or more above the actual rows"),
            (
                3,
                "the scan matches every node; consider the property index"
            ),
        ]),
        "Warnings: Id 1: estimated rows 10x or more above the actual rows; \
     Id 3: the scan matches every node; consider the property index"
    );
}
