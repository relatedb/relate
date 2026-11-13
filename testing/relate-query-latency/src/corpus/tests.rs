//! The corpus root's own tests: the status marker's grammar, the
//! query-file loader, the workload table's derived parts, and the
//! lane-shape derivations (report names, default timeouts) — the format
//! and table facts the lanes and the binary build on.

use super::*;
use std::str::FromStr;

#[test]
fn status_markers_classify_and_bad_markers_fail() {
    assert_eq!(
        Classification::parse_status("ok at SF0.1 (spike 2026-11-01)").unwrap(),
        Classification::Ok
    );
    assert_eq!(
        Classification::parse_status("kept — timeout at SF0.1 (cyclic recursion class)").unwrap(),
        Classification::Kept
    );
    assert_eq!(
        Classification::parse_status("error-pin UnsafeRepeatableElements — fail-closed").unwrap(),
        Classification::ErrorPin {
            error: "UnsafeRepeatableElements".into()
        }
    );
    assert!(Classification::parse_status("timeout at SF0.1").is_err());
    assert!(
        Classification::parse_status("ok").is_err(),
        "a marker needs its prose"
    );
    assert!(Classification::parse_status("error-pin").is_err());
    assert!(Classification::parse_status("error-pin — prose with no error name").is_err());
}

/// A workload directory holding the format's shapes, in temp dirs.
fn write_workload(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let queries = dir.path().join("queries");
    std::fs::create_dir_all(&queries).unwrap();
    for (name, body) in files {
        std::fs::write(queries.join(name), body).unwrap();
    }
    dir
}

const OK_FILE: &str = "// p1: A?\n// source: fixture\n// status: ok at SF0.1\nRETURN $seed AS s";
const KEPT_FILE: &str =
    "// r1: A+ [TRAIL]\n// status: kept — frontier-NFA case (LABS-644)\nRETURN 1";
const PIN_FILE: &str =
    "// r2: A+ [WALK-RE]\n// status: error-pin UnsafeRepeatableElements — fail-closed\nRETURN 1";

#[test]
fn query_files_load_with_their_classification() {
    let dir = write_workload(&[
        ("p001.cypher", OK_FILE),
        ("r001.cypher", KEPT_FILE),
        ("r002.cypher", PIN_FILE),
    ]);
    let queries = load_queries(dir.path(), "queries").unwrap();
    assert_eq!(queries.len(), 3);
    assert_eq!(queries[0].id, "p001");
    assert_eq!(queries[0].classification, Classification::Ok);
    assert_eq!(queries[0].text, "RETURN $seed AS s");
    assert_eq!(queries[0].sha256.len(), 64);
    assert_eq!(queries[1].classification, Classification::Kept);
    assert_eq!(
        queries[2].classification,
        Classification::ErrorPin {
            error: "UnsafeRepeatableElements".into()
        }
    );
}

#[test]
fn a_file_without_one_status_marker_is_rejected() {
    let dir = write_workload(&[("p001.cypher", "// only prose\nRETURN 1")]);
    assert!(load_queries(dir.path(), "queries")
        .unwrap_err()
        .contains("no `// status:` marker"));
    let dir = write_workload(&[("p001.cypher", "// status: ok\n// status: ok\nRETURN 1")]);
    assert!(load_queries(dir.path(), "queries")
        .unwrap_err()
        .contains("2 `// status:` markers"));
    let empty = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(empty.path().join("queries")).unwrap();
    assert!(load_queries(empty.path(), "queries").is_err());
}

/// The workload table's derived parts stay in step with its
/// variants: `ALL` holds every variant, in the table's order, and
/// every name round-trips through `FromStr`.
#[test]
fn the_all_list_is_the_variants() {
    for w in Workload::ALL {
        assert_eq!(Workload::from_str(w.as_str()).unwrap(), w);
        assert!(!w.dir().as_os_str().is_empty());
        assert_eq!(w.query_dirs().first(), Some(&"queries"));
    }
    assert!(Workload::from_str("nope").unwrap_err().contains("one of"));
}

#[test]
fn the_committed_workloads_load() {
    for workload in Workload::ALL {
        let queries = workload_queries(workload).unwrap();
        assert!(!queries.is_empty(), "{workload}");
        // Every corpus statement is a read: a MATCH, or (bi-16) the
        // UNWIND of its parameter maps a MATCH follows.
        assert!(
            queries
                .iter()
                .all(|q| q.text.starts_with("MATCH") || q.text.starts_with("UNWIND")),
            "{workload}: every corpus statement is a read"
        );
    }
    let pathdb = workload_queries(Workload::Pathdb).unwrap();
    assert_eq!(pathdb.len(), 207, "142 queries plus 65 restrictors");
    let count = |c: &Classification| pathdb.iter().filter(|q| &q.classification == c).count();
    assert_eq!(count(&Classification::Ok), 134);
    assert_eq!(count(&Classification::Kept), 66);
    assert_eq!(
        count(&Classification::ErrorPin {
            error: "UnsafeRepeatableElements".into()
        }),
        7
    );
    let fraud = workload_queries(Workload::FraudQpp).unwrap();
    assert_eq!(fraud.len(), 6);
    let snb_bi = workload_queries(Workload::SnbBi).unwrap();
    assert_eq!(
        snb_bi.len(),
        16,
        "bi-01..bi-18 minus bi-10 and bi-15 (bi-19 and bi-20 are skipped too, outside that \
         range)"
    );
    let count = |c: &Classification| snb_bi.iter().filter(|q| &q.classification == c).count();
    assert_eq!(count(&Classification::Ok), 11);
    assert_eq!(count(&Classification::Kept), 5, "the walk-budget class");
    let ukrailway = workload_queries(Workload::Ukrailway).unwrap();
    assert_eq!(ukrailway.len(), 2, "r02 and r03, both verified 2026-11-02");
    assert!(
        ukrailway
            .iter()
            .all(|q| q.classification == Classification::Ok),
        "both entries answer-checked against the verified oracle"
    );
    let agg_stress = workload_queries(Workload::AggStress).unwrap();
    assert_eq!(agg_stress.len(), 5, "a01..a05, all verified 2026-11-02");
    assert!(
        agg_stress
            .iter()
            .all(|q| q.classification == Classification::Ok),
        "every entry answer-checked against the verified oracle"
    );
}

/// One lane per workload, in [`Workload::ALL`] order — the ONE fixture
/// the lane-fact tests below build from: the match is exhaustive over
/// the workload vocabulary and the list derives from `ALL`, so a new
/// workload fails to compile here rather than leaving a test that
/// silently never sees its lane.
fn every_lane(sf: ScaleFactor) -> Vec<Lane> {
    Workload::ALL
        .iter()
        .map(|w| match w {
            Workload::Pathdb => Lane::Pathdb {
                graph: graph_opts(sf),
                seed_policy: SeedPolicy::Recorded,
            },
            Workload::FraudQpp => Lane::Fraud {
                fraud: FraudConfig::ci(),
            },
            Workload::SnbBi => Lane::SnbBi {
                graph: graph_opts(sf),
            },
            Workload::Ukrailway => Lane::Ukrailway {
                data_root: PathBuf::from("/nonexistent"),
                download: false,
            },
            Workload::AggStress => Lane::AggStress {
                graph: graph_opts(sf),
            },
            Workload::EquiJoin => Lane::EquiJoin {
                graph: graph_opts(sf),
            },
        })
        .collect()
}

/// The derived workload of each lane never disagrees with the lane
/// itself — the derivation is the only statement of the fact, and the
/// fixture's order is `ALL`'s own, so the pairing cannot drift.
#[test]
fn the_derived_workload_of_each_lane_is_the_lane_itself() {
    for (lane, w) in every_lane(ScaleFactor::Sf0_1).iter().zip(Workload::ALL) {
        assert_eq!(lane.workload(), w);
    }
}

fn graph_opts(sf: ScaleFactor) -> GraphOptions {
    GraphOptions {
        sf,
        profile: crate::workload::IndexProfile::Upstream,
        store: None,
        data_root: PathBuf::from("/nonexistent"),
        download: false,
    }
}

/// Each lane's default report name, byte-identical to the names the
/// binary writes: the graph-benchmark-ldbc-store lanes carry their own graph scale
/// factor in the name; the one-size lanes record none. Driven from the
/// one fixture, so a new lane is covered without touching this test.
#[test]
fn report_names_carry_the_workload_and_scale_factor() {
    for lane in every_lane(ScaleFactor::Sf0_1) {
        let w = lane.workload();
        let expected = match w {
            // The one-size lanes — fraud-qpp's generator knobs,
            // ukrailway's single-size data set — have no scale factor to
            // record.
            Workload::FraudQpp | Workload::Ukrailway => format!("report-corpus-{w}.json"),
            _ => format!("report-corpus-{w}-sf0.1.json"),
        };
        assert_eq!(lane.report_name(), expected, "{w}");
    }
    // The scale factor really is read from the lane: the same workload
    // at SF1 names its report for SF1.
    assert_eq!(
        (Lane::SnbBi {
            graph: graph_opts(ScaleFactor::Sf1),
        })
        .report_name(),
        "report-corpus-snb-bi-sf1.json"
    );
}

/// Each lane's default per-entry timeout is its own
/// [`Lane::default_timeout`], which reads its own facts — the one home
/// of every lane's bound, so no lane-class summary is restated here to
/// go stale when a lane joins. Driven from the one fixture, and each
/// lane's bound stated exactly once in the exhaustive match below.
#[test]
fn each_lanes_default_timeout_reads_its_own_facts() {
    // Each lane's own facts: the two table-recorded lanes scale their
    // bound off their graph (10 s at SF0.1), every other lane carries a
    // flat bound — its const's doc holds the why (the fraud graphs are
    // generator-sized; ukrailway's r02 is minutes-scale over the
    // imported graph; agg-stress's slowest entry measures 15.3 s;
    // equi-join's headline is product-dominated and machine-sensitive
    // at 7.2 s).
    for lane in every_lane(ScaleFactor::Sf0_1) {
        let want = match lane.workload() {
            Workload::Pathdb | Workload::SnbBi => Duration::from_secs(10),
            Workload::FraudQpp => Duration::from_secs(10),
            Workload::Ukrailway => Duration::from_secs(600),
            Workload::AggStress => Duration::from_secs(60),
            Workload::EquiJoin => Duration::from_secs(30),
        };
        assert_eq!(lane.default_timeout(), want, "{}", lane.workload());
    }
    // The scale factor really is read from the lane: the two
    // table-recorded lanes at SF1 bound themselves at 20 s.
    for lane in every_lane(ScaleFactor::Sf1) {
        if matches!(lane.workload(), Workload::Pathdb | Workload::SnbBi) {
            assert_eq!(
                lane.default_timeout(),
                Duration::from_secs(20),
                "{} at SF1",
                lane.workload()
            );
        }
    }
}
