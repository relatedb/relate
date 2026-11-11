use super::memory::{memory_point, per_count};
use super::*;
use rand::SeedableRng;

#[test]
#[should_panic(expected = "workload picker pool is non-empty by construction")]
fn pick_one_panics_on_an_empty_pool() {
    let mut rng = StdRng::seed_from_u64(1);
    let pool: [&str; 0] = [];
    let _ = pick_one(&mut rng, &pool);
}

#[test]
fn write_fraction_must_be_unit_interval() {
    assert!(validate_write_fraction(0.0).is_ok());
    assert!(validate_write_fraction(0.5).is_ok());
    assert!(validate_write_fraction(1.0).is_ok());
    assert!(validate_write_fraction(-0.01).is_err());
    assert!(validate_write_fraction(1.01).is_err());
    assert!(validate_write_fraction(f64::NAN).is_err());
}

#[test]
fn parse_cli_durability_mode_values() {
    assert_eq!(
        "full".parse::<DurabilityMode>().unwrap(),
        DurabilityMode::Full
    );
    assert_eq!(
        "EverySecond".parse::<DurabilityMode>().unwrap(),
        DurabilityMode::EverySecond
    );
    assert_eq!(
        "snapshot".parse::<DurabilityMode>().unwrap(),
        DurabilityMode::Snapshot
    );
    assert!("later".parse::<DurabilityMode>().is_err());
}

#[test]
fn omitted_durability_mode_flag_defaults_to_full() {
    use clap::Parser;
    #[derive(Parser)]
    struct Probe {
        #[command(flatten)]
        common: CommonArgs,
    }
    let probe = Probe::try_parse_from(["fraud-loadtest"]).unwrap();
    assert_eq!(probe.common.storage.durability_mode, DurabilityMode::Full);
    assert_eq!(
        LoadtestConfig::default().durability_mode,
        DurabilityMode::Full
    );
    assert_eq!(DurabilityMode::default(), DurabilityMode::Full);
}

#[test]
fn snapshot_scalars_flatten_in_json() {
    let snap = GraphSnapshot {
        nodes: 5,
        relationships: 7,
        scalars: BTreeMap::from([("accounts".into(), 3), ("customers".into(), 2)]),
    };
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(v["accounts"], 3);
    assert_eq!(v["nodes"], 5);
    let back: GraphSnapshot = serde_json::from_value(v).unwrap();
    assert_eq!(back.scalar("customers"), 2);
    assert_eq!(back.scalar("missing"), 0);
}

#[test]
fn knobs_serialize_untagged() {
    let f = serde_json::to_value(Knobs::Fraud {
        fanout_k: 12,
        cycle_len: 4,
    })
    .unwrap();
    assert_eq!(f, serde_json::json!({"fanout_k": 12, "cycle_len": 4}));
    let i = serde_json::to_value(Knobs::Identity {
        nest_depth: 4,
        group_k: 12,
    })
    .unwrap();
    assert_eq!(i, serde_json::json!({"nest_depth": 4, "group_k": 12}));
    let back: Knobs = serde_json::from_value(i).unwrap();
    assert_eq!(
        back,
        Knobs::Identity {
            nest_depth: 4,
            group_k: 12
        }
    );
}

#[test]
fn memory_report_scales_with_graph_size() {
    let after_open = MemoryPoint {
        rss_bytes: 10_000_000,
        peak_rss_bytes: 10_000_000,
        ..MemoryPoint::default()
    };
    let start_size = GraphSnapshot {
        nodes: 100,
        relationships: 400,
        scalars: BTreeMap::from([("accounts".into(), 80), ("customers".into(), 20)]),
    };
    let finish_size = GraphSnapshot {
        nodes: 102,
        relationships: 410,
        scalars: BTreeMap::from([("accounts".into(), 81), ("customers".into(), 21)]),
    };
    let start = sample_from(50_000_000, &start_size, "accounts");
    let finish = sample_from(50_500_000, &finish_size, "accounts");
    let mem = build_memory_report(
        after_open,
        start,
        finish,
        &start_size,
        &finish_size,
        "accounts",
    );
    assert_eq!(mem.graph.rss_bytes, 40_000_000);
    assert!((mem.graph.bytes_per_entity - 40_000_000.0 / 500.0).abs() < 1e-9);
    assert!((mem.graph.bytes_per_unit - 40_000_000.0 / 80.0).abs() < 1e-9);
    assert_eq!(mem.workload_delta.rss_bytes, 500_000);
    assert_eq!(mem.workload_delta.nodes, 2);
    assert_eq!(mem.workload_delta.relationships, 10);
    assert!((mem.workload_delta.rss_bytes_per_new_entity - 500_000.0 / 12.0).abs() < 1e-9);
}

fn sample_from(rss: u64, snap: &GraphSnapshot, unit: &str) -> MemoryPoint {
    memory_point(rss, rss, Some(snap), unit)
}

#[test]
fn per_count_guards_non_positive_denominators() {
    assert_eq!(per_count(10.0, 0), 0.0);
    assert_eq!(per_count(10.0, -3), 0.0);
    assert_eq!(per_count(-8.0, 4), -2.0);
    assert_eq!(per_count(9.0, 3), 3.0);
}

#[test]
fn memory_point_without_snapshot_has_zero_ratios() {
    let point = memory_point(4096, 8192, None, "accounts");
    assert_eq!(point.rss_bytes, 4096);
    assert_eq!(point.peak_rss_bytes, 8192);
    assert_eq!(point.rss_per_node, 0.0);
    assert_eq!(point.rss_per_relationship, 0.0);
    assert_eq!(point.rss_per_entity, 0.0);
    assert_eq!(point.rss_per_unit, 0.0);
}
