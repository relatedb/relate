use std::path::PathBuf;
use std::time::Duration;

use clap::{Args as ClapArgs, Parser, Subcommand};
use relate_datasets::fraud::FraudConfig;
use relate_query_latency::corpus::{run_corpus, CorpusConfig, Lane, SeedPolicy, StoreWorkload};
use relate_query_latency::fetch::{data_root, ensure_dataset};
use relate_query_latency::profile::{profile, ProfileOptions, Sampler};
use relate_query_latency::run::RunConfig;
use relate_query_latency::suite::{report_name, run_suite, GraphOptions, SuiteOptions};
use relate_query_latency::work_root;
use relate_query_latency::workload::{IndexProfile, ScaleFactor, QIDS};
use relate_test_support::profiler::DEFAULT_FREQUENCY;

#[derive(Parser, Debug)]
#[command(
    name = "query-latency",
    about = "Query latency on the graph-benchmark-ldbc workload (ADR-0243; not an LDBC Benchmark)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

/// The data cache flag, shared by every subcommand that reads the data
/// set. The `RELATE_QUERY_LATENCY_DATA` fallback lives in
/// [`data_root`], not the clap arg — a relocated cache via the env var
/// is not a passed `--data`.
#[derive(ClapArgs, Debug)]
struct DataArgs {
    /// Data cache directory (default: $RELATE_QUERY_LATENCY_DATA,
    /// else target/query-latency/data).
    #[arg(long = "data")]
    dir: Option<PathBuf>,
}

impl DataArgs {
    /// The one default resolution of the data-cache directory: the
    /// passed flag, else the env var, else the work-root default —
    /// every `--data` reader resolves through here.
    fn root(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(data_root)
    }
}

/// The graph-benchmark-ldbc-store flags, shared by the workloads that
/// run over the graph-benchmark-ldbc store: the [`CorpusCommand`]
/// subcommands that flatten this group are the one home of which
/// workloads those are.
#[derive(ClapArgs, Debug)]
struct GraphBenchmarkLdbcStoreArgs {
    /// Scale factor: pathdb's params file and the store it loads — SF1 runs the
    /// 26-query subset recorded for it; snb-bi's params file and store,
    /// recorded at SF0.1 only; agg-stress's and equi-join's oracle
    /// files and stores, recorded at SF0.1 only.
    #[arg(long, default_value = "0.1")]
    sf: ScaleFactor,
    /// Reuse the graph-benchmark-ldbc store at PATH — opened
    /// read-only; when PATH does not exist, load into it, checkpoint it
    /// and mark it complete for the next run (same marker rules as
    /// `run`).
    #[arg(long)]
    store: Option<PathBuf>,
}

impl GraphBenchmarkLdbcStoreArgs {
    /// The graph-benchmark-ldbc-store graph options a corpus run opens its store with —
    /// the group's own conversion (the `GraphArgs::into_options`
    /// shape): upstream indexes, the group's own scale factor and store
    /// and the data-cache knobs.
    fn graph_options(self, data: &DataLaneArgs) -> GraphOptions {
        GraphOptions {
            sf: self.sf,
            profile: IndexProfile::Upstream,
            store: self.store,
            data_root: data.root(),
            download: data.downloads(),
            ..GraphOptions::default()
        }
    }
}

/// The data-cache flags, shared by the corpus workloads that fetch a
/// data set: the [`CorpusCommand`] subcommands that flatten this group
/// are the one home of which workloads those are. The shared `--data`
/// is [`DataArgs`], flattened; only `--offline` is the group's own.
#[derive(ClapArgs, Debug)]
struct DataLaneArgs {
    #[command(flatten)]
    data: DataArgs,
    /// Fail instead of downloading a data set that is not cached.
    #[arg(long)]
    offline: bool,
}

impl DataLaneArgs {
    /// The data-cache directory, resolved by [`DataArgs::root`].
    fn root(&self) -> PathBuf {
        self.data.root()
    }

    /// Whether a missing or mismatched file may be downloaded.
    fn downloads(&self) -> bool {
        !self.offline
    }
}

/// The pathdb lane's own flags (`corpus pathdb`).
#[derive(ClapArgs, Debug)]
struct PathdbLaneArgs {
    /// Seed policy: median re-resolves every seed key
    /// with the degree-median policy and reports the diffs; recorded
    /// runs the pinned params file as-is.
    #[arg(long, default_value = "median")]
    seed_policy: SeedPolicy,
}

/// The fraud lane's flags (`corpus fraud-qpp`).
#[derive(ClapArgs, Debug)]
struct FraudLaneArgs {
    /// Fraud generator scale: 1.0 = 10,000 accounts.
    /// Default: the datasets crate's env-gated scale
    /// (RELATE_FULL_DATA=1 → 1.0, else the CI scale 0.05).
    #[arg(long)]
    fraud_scale: Option<f64>,
    /// Fraud generator seed: planted ids are stable
    /// per seed. Default: the datasets crate's CI seed.
    #[arg(long)]
    fraud_seed: Option<u64>,
}

impl FraudLaneArgs {
    /// The fraud generator's knobs — the group's own conversion (the
    /// `GraphArgs::into_options` shape): the datasets crate's env-gated
    /// base (`RELATE_FULL_DATA=1` opts up to the generator's full
    /// scale, the same gate the corpus test runs under), with this
    /// group's flags overriding its knobs.
    fn into_config(self) -> FraudConfig {
        let mut config = FraudConfig::from_env();
        if let Some(scale) = self.fraud_scale {
            config.scale = scale;
        }
        if let Some(seed) = self.fraud_seed {
            config.seed = seed;
        }
        config
    }
}

/// The graph flags, shared by every subcommand that queries the graph.
#[derive(ClapArgs, Debug)]
struct GraphArgs {
    /// Scale factor: 0.1 or 1.
    #[arg(long, default_value = "0.1")]
    sf: ScaleFactor,
    /// Index profile: upstream (ID plus eight lookup indexes) or text
    /// (upstream plus three TEXT indexes).
    #[arg(long, default_value = "upstream")]
    indexes: IndexProfile,
    /// Reuse the store at PATH (opened read-only); when PATH does not
    /// exist, load into it, checkpoint it and mark it complete in
    /// PATH.query-latency.json for the next run, which requires that marker
    /// and the same --sf and --indexes. Without it the graph is loaded into
    /// :memory: every run.
    #[arg(long)]
    store: Option<PathBuf>,
    #[command(flatten)]
    data: DataArgs,
    /// Fail instead of downloading a data set that is not cached.
    #[arg(long)]
    offline: bool,
    /// Per-execution timeout in seconds (default: 60 at SF0.1, 300 at
    /// SF1). A query past it is cancelled.
    #[arg(long, value_parser = seconds_parser)]
    timeout: Option<Duration>,
}

impl GraphArgs {
    /// The graph options and the per-execution timeout.
    fn into_options(self) -> (GraphOptions, Duration) {
        let timeout = self.timeout.unwrap_or_else(|| self.sf.default_timeout());
        let graph = GraphOptions {
            sf: self.sf,
            profile: self.indexes,
            store: self.store,
            data_root: self.data.root(),
            download: !self.offline,
            ..GraphOptions::default()
        };
        (graph, timeout)
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Download, verify and extract the data set into the data cache.
    Fetch {
        /// Scale factor: 0.1 or 1.
        #[arg(long, default_value = "0.1")]
        sf: ScaleFactor,
        #[command(flatten)]
        data: DataArgs,
    },
    /// Load the graph, then check and time each query; writes the JSON
    /// report and prints a table. Exits 1 when any query is not ok.
    Run {
        #[command(flatten)]
        graph: GraphArgs,
        /// Comma-separated query ids, 1-30 (default: all 30).
        #[arg(long, value_delimiter = ',', value_parser = qid_parser())]
        qids: Vec<u32>,
        /// Timed rounds per query, at least.
        #[arg(long, default_value_t = 5)]
        min_rounds: usize,
        /// Seconds of timed rounds per query, at least.
        #[arg(long, default_value = "1", value_parser = seconds_parser)]
        min_time: Duration,
        /// Report JSON path (default:
        /// target/query-latency/report-graph-benchmark-ldbc-sf<SF>.json).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Load the graph, check one query's answer, then loop it under a
    /// sampling profiler and write top.txt, folded.txt, flame.svg and
    /// summary.json (ADR-0244).
    Profile {
        #[command(flatten)]
        graph: GraphArgs,
        /// The query id to profile (1-30).
        #[arg(long, value_parser = qid_parser())]
        qid: u32,
        /// Seconds to loop the query (at least one iteration runs).
        #[arg(long, default_value = "10", value_parser = seconds_parser)]
        seconds: Duration,
        /// Samples per second of CPU time.
        #[arg(long, default_value_t = DEFAULT_FREQUENCY,
              value_parser = clap::value_parser!(i32).range(1..))]
        frequency: i32,
        /// pprof (in-process) or none (loop only, for samply or perf).
        #[arg(long, default_value = "pprof")]
        sampler: Sampler,
        /// Output directory (default:
        /// target/query-latency/profile/graph-benchmark-ldbc-sf<SF>-q<NN>[-text]).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run a QPP/LDBC corpus workload (ADR-0479, LABS-646), one
    /// subcommand per workload, each carrying only the flags its lane
    /// runs. `ok` entries must run (and answer-match where an oracle
    /// exists), `kept` entries must plan, `error-pin` entries must fail
    /// with their authored error. Exit 1 when any entry misses its
    /// expectation.
    Corpus {
        #[command(subcommand)]
        workload: CorpusCommand,
    },
}

/// The flags every corpus workload runs under.
#[derive(ClapArgs, Debug)]
struct CorpusRunArgs {
    /// Per-entry timeout in seconds (default: the lane's own
    /// per-entry bound, which reads its own facts — see
    /// `Lane::default_timeout`; a timed-out `ok` entry fails the
    /// run — the ratchet).
    #[arg(long, value_parser = seconds_parser)]
    timeout: Option<Duration>,
    /// Extra timed executions of entries faster than 1.5 s, for the
    /// min/median columns (0 measures once).
    #[arg(long, default_value_t = 2)]
    repeats: usize,
    /// Report JSON path (default:
    /// target/query-latency/report-corpus-<workload>[-sf<SF>].json).
    #[arg(long)]
    out: Option<PathBuf>,
}

/// The corpus workloads, one subcommand each: each flattens only the
/// flag groups its lane serves, so clap itself refuses a flag of
/// another lane — the subcommand's own argument list is the one home of
/// which flags belong to which workload. The subcommand names are the
/// [`Workload::as_str`](relate_query_latency::corpus::Workload::as_str)
/// names (a test pins the correspondence).
#[derive(Subcommand, Debug)]
enum CorpusCommand {
    /// The PathDB RPQ translations over the graph-benchmark-ldbc store.
    Pathdb {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
        #[command(flatten)]
        data: DataLaneArgs,
        #[command(flatten)]
        pathdb: PathdbLaneArgs,
    },
    /// The LDBC SNB BI analytics queries over the graph-benchmark-ldbc
    /// store.
    SnbBi {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
        #[command(flatten)]
        data: DataLaneArgs,
    },
    /// The in-house fraud QPP suite over generated and fixture graphs.
    FraudQpp {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        fraud: FraudLaneArgs,
    },
    /// The verified UK railway graph queries over the workload's own
    /// imported graph.
    Ukrailway {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        data: DataLaneArgs,
    },
    /// The in-house aggregation-stress queries over the
    /// graph-benchmark-ldbc store.
    AggStress {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
        #[command(flatten)]
        data: DataLaneArgs,
    },
    /// The in-house equi-join queries over the graph-benchmark-ldbc
    /// store.
    EquiJoin {
        #[command(flatten)]
        run: CorpusRunArgs,
        #[command(flatten)]
        graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
        #[command(flatten)]
        data: DataLaneArgs,
    },
}

impl CorpusCommand {
    /// The lane the chosen workload runs and the shared run flags: each
    /// arm reads only the groups its subcommand declares, so no lane
    /// ever sees a flag it does not own. The lane is built first and its
    /// policies (default timeout, report name) resolve off the built
    /// value — each carries its own facts (the store lanes' graph scale
    /// factor, the fraud lane's generator knobs, the ukrailway lane's
    /// data-cache knobs).
    fn into_lane(self) -> (Lane, CorpusRunArgs) {
        match self {
            Self::Pathdb {
                run,
                graph_benchmark_ldbc,
                data,
                pathdb,
            } => (
                Lane::Pathdb {
                    graph: graph_benchmark_ldbc.graph_options(&data),
                    seed_policy: pathdb.seed_policy,
                },
                run,
            ),
            Self::SnbBi {
                run,
                graph_benchmark_ldbc,
                data,
            } => store_lane(StoreWorkload::SnbBi, run, graph_benchmark_ldbc, &data),
            Self::AggStress {
                run,
                graph_benchmark_ldbc,
                data,
            } => store_lane(StoreWorkload::AggStress, run, graph_benchmark_ldbc, &data),
            Self::EquiJoin {
                run,
                graph_benchmark_ldbc,
                data,
            } => store_lane(StoreWorkload::EquiJoin, run, graph_benchmark_ldbc, &data),
            Self::FraudQpp { run, fraud } => (
                Lane::Fraud {
                    fraud: fraud.into_config(),
                },
                run,
            ),
            Self::Ukrailway { run, data } => (
                Lane::Ukrailway {
                    data_root: data.root(),
                    download: data.downloads(),
                },
                run,
            ),
        }
    }
}

/// The lane of a workload that shares the graph-benchmark-ldbc store
/// payload: the one place the store arms of [`CorpusCommand::into_lane`]
/// build it.
fn store_lane(
    workload: StoreWorkload,
    run: CorpusRunArgs,
    graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
    data: &DataLaneArgs,
) -> (Lane, CorpusRunArgs) {
    (
        Lane::GraphStore {
            workload,
            graph: graph_benchmark_ldbc.graph_options(data),
        },
        run,
    )
}

/// A query id: one of [`QIDS`].
fn qid_parser() -> clap::builder::RangedI64ValueParser<u32> {
    clap::value_parser!(u32).range(i64::from(*QIDS.start())..=i64::from(*QIDS.end()))
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("query-latency: {err}");
            std::process::exit(2);
        }
    }
}

/// A non-negative number of seconds (`1`, `0.5`) as a [`Duration`].
fn seconds_parser(s: &str) -> Result<Duration, String> {
    let secs: f64 = s
        .parse()
        .map_err(|_| format!("{s:?} is not a number of seconds"))?;
    Duration::try_from_secs_f64(secs)
        .map_err(|_| format!("{s:?} is not a non-negative number of seconds"))
}

fn run() -> Result<i32, String> {
    let args = Args::parse();
    match args.command {
        Command::Fetch { sf, data } => {
            let dir = ensure_dataset(&data.root(), sf, true)?;
            println!("{}", dir.display());
            Ok(0)
        }
        Command::Run {
            graph,
            qids,
            min_rounds,
            min_time,
            out,
        } => {
            let qids = if qids.is_empty() {
                QIDS.collect()
            } else {
                qids
            };
            let (graph, timeout) = graph.into_options();
            let sf = graph.sf;
            let opts = SuiteOptions {
                graph,
                qids,
                run: RunConfig {
                    min_rounds,
                    min_time,
                    ..RunConfig::new(timeout)
                },
            };
            let report = run_suite(&opts)?;
            let out = out.unwrap_or_else(|| work_root().join(report_name(sf)));
            finish_report(
                &out,
                report.write_json(&out),
                &report.table(),
                !report.failures().is_empty(),
            )
        }
        Command::Profile {
            graph,
            qid,
            seconds,
            frequency,
            sampler,
            out,
        } => {
            let (graph, timeout) = graph.into_options();
            let summary = profile(&ProfileOptions {
                graph,
                qid,
                seconds,
                frequency,
                sampler,
                timeout,
                out_dir: out,
            })?;
            println!("{}", summary.out_dir);
            Ok(0)
        }
        Command::Corpus { workload } => {
            let (lane, run) = workload.into_lane();
            let timeout = run.timeout.unwrap_or_else(|| lane.default_timeout());
            let config = CorpusConfig {
                lane,
                timeout,
                repeats: run.repeats,
            };
            let report = run_corpus(&config)?;
            let out = run
                .out
                .unwrap_or_else(|| work_root().join(config.lane.report_name()));
            finish_report(
                &out,
                report.write_json(&out),
                &report.table(),
                !report.failures().is_empty(),
            )
        }
    }
}

/// The report-finish tail every timed run shares: the JSON write's
/// outcome, the table on stdout, the report path on stderr, and the exit
/// code (1 when any entry failed).
fn finish_report(
    out: &std::path::Path,
    written: Result<(), String>,
    table: &str,
    failed: bool,
) -> Result<i32, String> {
    written?;
    print!("{table}");
    eprintln!("query-latency: wrote {}", out.display());
    Ok(i32::from(failed))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use relate_query_latency::corpus::Workload;

    use super::*;

    /// The corpus subcommand's parse of a raw argv.
    fn parse_corpus(argv: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("query-latency").chain(argv.iter().copied()))
    }

    /// The corpus subcommands ARE the workload vocabulary: one
    /// subcommand per [`Workload`], named by [`Workload::as_str`] —
    /// a workload added without its subcommand (or a subcommand named
    /// apart from its workload) fails here.
    #[test]
    fn corpus_subcommands_are_the_workload_vocabulary() {
        let cmd = Args::command();
        let corpus = cmd
            .find_subcommand("corpus")
            .expect("the corpus subcommand");
        let mut names: Vec<&str> = corpus.get_subcommands().map(|c| c.get_name()).collect();
        let mut want: Vec<&str> = Workload::ALL.iter().map(|w| w.as_str()).collect();
        names.sort_unstable();
        want.sort_unstable();
        assert_eq!(names, want);
    }

    /// Each subcommand carries only its own lane's flags: a flag of
    /// another lane is a clap parse error (a mistyped run, never a
    /// silently ignored knob), and a flag the lane owns parses.
    #[test]
    fn each_workload_accepts_only_its_own_lane_flags() {
        for ok in [
            vec![
                "corpus",
                "pathdb",
                "--sf",
                "0.1",
                "--seed-policy",
                "recorded",
            ],
            vec!["corpus", "pathdb", "--store", "/tmp/s.rl", "--offline"],
            vec!["corpus", "snb-bi", "--sf", "0.1", "--store", "/tmp/s.rl"],
            vec![
                "corpus",
                "fraud-qpp",
                "--fraud-scale",
                "0.05",
                "--fraud-seed",
                "7",
            ],
            vec!["corpus", "ukrailway", "--data", "/tmp/cache", "--offline"],
            vec![
                "corpus",
                "agg-stress",
                "--sf",
                "0.1",
                "--data",
                "/tmp/cache",
            ],
            vec!["corpus", "equi-join", "--timeout", "5", "--repeats", "0"],
        ] {
            assert!(parse_corpus(&ok).is_ok(), "{ok:?} is a clean command line");
        }
        for bad in [
            vec!["corpus", "fraud-qpp", "--sf", "0.1"],
            vec!["corpus", "fraud-qpp", "--store", "/tmp/s.rl"],
            vec!["corpus", "fraud-qpp", "--data", "/tmp/cache"],
            vec!["corpus", "fraud-qpp", "--offline"],
            vec!["corpus", "ukrailway", "--sf", "0.1"],
            vec!["corpus", "ukrailway", "--store", "/tmp/s.rl"],
            vec!["corpus", "snb-bi", "--seed-policy", "recorded"],
            vec!["corpus", "agg-stress", "--seed-policy", "recorded"],
            vec!["corpus", "equi-join", "--fraud-scale", "1"],
            vec!["corpus", "pathdb", "--fraud-seed", "1"],
        ] {
            assert!(parse_corpus(&bad).is_err(), "{bad:?} is a wrong-lane flag");
        }
        // The workload is required: there is no default lane.
        assert!(parse_corpus(&["corpus"]).is_err());
    }

    /// Each subcommand builds the lane of its own workload — the lane's
    /// derived workload is the subcommand's name.
    #[test]
    fn each_subcommand_builds_its_own_lane() {
        for w in Workload::ALL {
            let args = parse_corpus(&["corpus", w.as_str()]).unwrap();
            let Command::Corpus { workload } = args.command else {
                panic!("the corpus subcommand");
            };
            let (lane, _) = workload.into_lane();
            assert_eq!(lane.workload(), w);
        }
    }
}
