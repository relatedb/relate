use std::path::PathBuf;
use std::time::Duration;

use clap::parser::ValueSource;
use clap::{
    ArgMatches, Args as ClapArgs, Command as ClapCommand, CommandFactory, FromArgMatches, Parser,
    Subcommand,
};
use relate_datasets::fraud::FraudConfig;
use relate_query_latency::corpus::{run_corpus, CorpusConfig, Lane, SeedPolicy, Workload};
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
/// is not a passed `--data` (the corpus lane check reads passed-ness
/// from clap's value_source).
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
    fn root_of(dir: Option<PathBuf>) -> PathBuf {
        dir.unwrap_or_else(data_root)
    }

    fn root(self) -> PathBuf {
        Self::root_of(self.dir)
    }
}

/// The graph-benchmark-ldbc-store flags, shared by the workloads that
/// run over the graph-benchmark-ldbc store (`--workload pathdb`,
/// `--workload snb-bi` and `--workload agg-stress`): the group's own
/// clap arguments are the one home of which flags belong to which lane
/// — the wrong-lane check enumerates them from here, never a second
/// table.
#[derive(ClapArgs, Debug)]
struct GraphBenchmarkLdbcStoreArgs {
    /// Scale factor (pathdb, snb-bi and agg-stress only): pathdb's
    /// params file and the store it loads — SF1 runs the 26-query
    /// subset recorded for it; snb-bi's params file and store,
    /// recorded at SF0.1 only; agg-stress's oracle file and
    /// store, recorded at SF0.1 only.
    #[arg(long, default_value = "0.1")]
    sf: ScaleFactor,
    /// Reuse the graph-benchmark-ldbc store at PATH (pathdb, snb-bi and agg-stress
    /// only) — opened
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
        }
    }
}

/// The data-cache flags, shared by the corpus workloads that fetch a
/// data set (`--workload pathdb`, `--workload snb-bi`,
/// `--workload ukrailway` and `--workload agg-stress`): the group's own clap
/// arguments are the one home of which
/// flags belong to which lanes — the wrong-lane check enumerates them
/// from here, never a second table. The `RELATE_QUERY_LATENCY_DATA`
/// fallback lives in [`data_root`], not the clap arg — a relocated cache
/// via the env var is not a passed `--data` (the corpus lane check reads
/// passed-ness from clap's value_source).
#[derive(ClapArgs, Debug)]
struct DataLaneArgs {
    /// Data cache directory the data sets fetch and load from (pathdb,
    /// snb-bi, ukrailway and agg-stress only). Default:
    /// $RELATE_QUERY_LATENCY_DATA,
    /// else target/query-latency/data.
    #[arg(long = "data")]
    data: Option<PathBuf>,
    /// Fail instead of downloading a data set that is not cached
    /// (pathdb, snb-bi, ukrailway and agg-stress only).
    #[arg(long)]
    offline: bool,
}

impl DataLaneArgs {
    /// The one default resolution of the data-cache directory — the same
    /// resolution [`DataArgs::root_of`] runs for the shared `--data`.
    fn root(&self) -> PathBuf {
        DataArgs::root_of(self.data.clone())
    }

    /// Whether a missing or mismatched file may be downloaded.
    fn downloads(&self) -> bool {
        !self.offline
    }
}

/// The pathdb lane's own flags (`--workload pathdb`): the group's own
/// clap arguments are the one home of which flags belong to this lane —
/// the wrong-lane check enumerates them from here, never a second
/// table.
#[derive(ClapArgs, Debug)]
struct PathdbLaneArgs {
    /// Seed policy (pathdb only): median re-resolves every seed key
    /// with the degree-median policy and reports the diffs; recorded
    /// runs the pinned params file as-is.
    #[arg(long, default_value = "median")]
    seed_policy: SeedPolicy,
}

/// The fraud lane's flags (`--workload fraud-qpp`): the group's own
/// clap arguments are the one home of which flags belong to this lane —
/// the wrong-lane check enumerates them from here, never a second
/// table.
#[derive(ClapArgs, Debug)]
struct FraudLaneArgs {
    /// Fraud generator scale (fraud-qpp only): 1.0 = 10,000 accounts.
    /// Default: the datasets crate's env-gated scale
    /// (RELATE_FULL_DATA=1 → 1.0, else the CI scale 0.05).
    #[arg(long)]
    fraud_scale: Option<f64>,
    /// Fraud generator seed (fraud-qpp only): planted ids are stable
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
    /// Run a QPP/LDBC corpus workload (ADR-0479, LABS-646): the PathDB
    /// RPQ translations, the LDBC SNB BI analytics queries or the
    /// in-house aggregation-stress queries over the graph-benchmark-ldbc store, the
    /// in-house fraud QPP suite over generated and fixture graphs, or
    /// the verified UK railway graph queries over the workload's own
    /// imported graph. `ok` entries must run (and
    /// answer-match where an oracle exists), `kept` entries must plan,
    /// `error-pin` entries must fail with their authored error. Exit 1
    /// when any entry misses its expectation.
    Corpus {
        /// The workload: pathdb, snb-bi, fraud-qpp, ukrailway or
        /// agg-stress.
        #[arg(long, default_value = "pathdb")]
        workload: Workload,
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
        #[command(flatten)]
        graph_benchmark_ldbc: GraphBenchmarkLdbcStoreArgs,
        #[command(flatten)]
        data: DataLaneArgs,
        #[command(flatten)]
        pathdb: PathdbLaneArgs,
        #[command(flatten)]
        fraud: FraudLaneArgs,
        /// Report JSON path (default:
        /// target/query-latency/report-corpus-<workload>[-sf<SF>].json).
        #[arg(long)]
        out: Option<PathBuf>,
    },
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

/// One lane group of the corpus subcommand: the group's own clap
/// declaration (the one home of which flags belong to which lane) and
/// the workloads it serves — the one fact each row carries. The
/// refusal's lane name and (tests only) the help tag are renderings of
/// `served`, derived in [`LaneGroup::new`], never stated per row. The
/// wrong-lane check and the help-tag test both iterate THIS table,
/// never a second hand-maintained one.
struct LaneGroup {
    cmd: ClapCommand,
    served: &'static [Workload],
    name: String,
    #[cfg(test)]
    tag: String,
}

impl LaneGroup {
    /// One row from the fact it carries: `name` reads back as `the
    /// <workloads> lane(s)` in the wrong-lane refusal, the test-only
    /// `tag` as `(<workloads> only)` in each flag's help — the workloads
    /// joined `a and b` for two, `a, b and c` for more. A group with no
    /// served workload is impossible by construction (every row of
    /// [`lane_groups`] serves at least one) and enforced here, never
    /// papered over with a fabricated lane name.
    fn new(cmd: ClapCommand, served: &'static [Workload]) -> Self {
        let names: Vec<&str> = served.iter().map(|w| w.as_str()).collect();
        // `split_last` yields the last element and the head before it.
        let (last, head) = names
            .split_last()
            .expect("a lane group serves at least one workload");
        let joined = if head.is_empty() {
            (*last).to_string()
        } else {
            format!("{} and {last}", head.join(", "))
        };
        Self {
            cmd,
            served,
            name: format!(
                "the {joined} lane{}",
                if served.len() > 1 { "s" } else { "" }
            ),
            #[cfg(test)]
            tag: format!("({joined} only)"),
        }
    }
}

/// The lane groups, one row per flag group of the corpus subcommand:
/// the graph-benchmark-ldbc-store group (the workloads over the graph-benchmark-ldbc
/// store — the scale factor their tables run at and the store they
/// reuse), the data-cache group (the workloads that fetch a data set —
/// the cache directory and the offline refusal), the pathdb group, the
/// fraud group. The `served` arrays below are the one home of the
/// membership; this prose never counts it.
fn lane_groups() -> Vec<LaneGroup> {
    vec![
        LaneGroup::new(
            GraphBenchmarkLdbcStoreArgs::augment_args(ClapCommand::new(
                "graph-benchmark-ldbc-store",
            )),
            &[Workload::Pathdb, Workload::SnbBi, Workload::AggStress],
        ),
        LaneGroup::new(
            DataLaneArgs::augment_args(ClapCommand::new("data-cache")),
            &[
                Workload::Pathdb,
                Workload::SnbBi,
                Workload::Ukrailway,
                Workload::AggStress,
            ],
        ),
        LaneGroup::new(
            PathdbLaneArgs::augment_args(ClapCommand::new("pathdb")),
            &[Workload::Pathdb],
        ),
        LaneGroup::new(
            FraudLaneArgs::augment_args(ClapCommand::new("fraud")),
            &[Workload::FraudQpp],
        ),
    ]
}

/// Each lane carries only its own flags: a flag of a lane the workload
/// does not run is a mistyped run, refused loudly — never a silently
/// ignored knob. Which flags belong to which lane has one home — the
/// lane groups' own clap arguments, enumerated with the workloads each
/// group serves in [`lane_groups`] — and passed-ness is clap's own fact
/// (`value_source`), read from the corpus subcommand's raw matches:
/// never a second hand-maintained table.
fn refuse_wrong_lane(workload: Workload, matches: &ArgMatches) -> Result<(), String> {
    let passed_of = |cmd: &ClapCommand| -> Vec<String> {
        cmd.get_arguments()
            .filter(|arg| {
                matches.value_source(arg.get_id().as_str()) == Some(ValueSource::CommandLine)
            })
            .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
            .collect()
    };
    for group in lane_groups() {
        let passed = passed_of(&group.cmd);
        if !passed.is_empty() && !group.served.contains(&workload) {
            return Err(format!(
                "{} configure(s) {} only, not --workload {workload}",
                passed.join(" "),
                group.name
            ));
        }
    }
    Ok(())
}

/// The corpus subcommand's workload, read from its RAW matches — the one
/// typed value the wrong-lane check needs before the typed extraction
/// consumes them. The flag's clap default makes the value always present.
fn workload_of(matches: &ArgMatches) -> Workload {
    *matches
        .get_one::<Workload>("workload")
        .expect("the corpus subcommand carries --workload")
}

fn run() -> Result<i32, String> {
    let mut matches = Args::command().get_matches();
    // The wrong-lane refusal reads passed-ness from the corpus
    // subcommand's raw matches, before the typed extraction consumes
    // them — a mistyped command line fails before any lane builds.
    if let Some(corpus) = matches.subcommand_matches("corpus") {
        refuse_wrong_lane(workload_of(corpus), corpus)?;
    }
    let args = Args::from_arg_matches_mut(&mut matches).unwrap_or_else(|e| e.exit());
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
            report.write_json(&out)?;
            print!("{}", report.table());
            eprintln!("query-latency: wrote {}", out.display());
            Ok(if report.failures().is_empty() { 0 } else { 1 })
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
        Command::Corpus {
            workload,
            timeout,
            repeats,
            graph_benchmark_ldbc,
            data,
            pathdb,
            fraud,
            out,
        } => {
            // The lane is built first, then its policies resolve off the
            // built value: each arm carries its own facts — the
            // graph-benchmark-ldbc-store lanes' graph scale factor, the fraud lane's
            // generator knobs, the ukrailway lane's data-cache knobs —
            // so the default timeout and the report name read the lane,
            // never a scale factor resolved before it (a passed `--sf`
            // on a fraud or ukrailway run was already refused before the
            // typed extraction; only the graph-benchmark-ldbc-store lanes carry one).
            // The graph-benchmark-ldbc-store lanes' one shared derivation,
            // built once: each store arm below takes it, and the fraud and
            // ukrailway arms never see a graph.
            let graph = graph_benchmark_ldbc.graph_options(&data);
            let lane = match workload {
                Workload::Pathdb => Lane::Pathdb {
                    graph,
                    // The lane owns the policy; the other arms carry
                    // none, so their runs never record a policy they
                    // cannot run (and a passed `--seed-policy` was
                    // already refused above).
                    seed_policy: pathdb.seed_policy,
                },
                Workload::SnbBi => Lane::SnbBi { graph },
                Workload::AggStress => Lane::AggStress { graph },
                Workload::FraudQpp => Lane::Fraud {
                    fraud: fraud.into_config(),
                },
                Workload::Ukrailway => Lane::Ukrailway {
                    data_root: data.root(),
                    download: data.downloads(),
                },
            };
            let timeout = timeout.unwrap_or_else(|| lane.default_timeout());
            let config = CorpusConfig {
                lane,
                timeout,
                repeats,
            };
            let report = run_corpus(&config)?;
            let out = out.unwrap_or_else(|| work_root().join(config.lane.report_name()));
            report.write_json(&out)?;
            print!("{}", report.table());
            eprintln!("query-latency: wrote {}", out.display());
            Ok(if report.failures().is_empty() { 0 } else { 1 })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// The corpus subcommand's matches, parsed from a raw argv — the
    /// same matches `run()` hands the wrong-lane check. Accepts owned
    /// and borrowed argv spellings alike; every item becomes one argv
    /// word.
    fn corpus_matches<S: Into<OsString> + Clone>(argv: &[S]) -> ArgMatches {
        Args::command()
            .try_get_matches_from(
                std::iter::once(OsString::from("query-latency"))
                    .chain(argv.iter().cloned().map(Into::into)),
            )
            .unwrap()
            .subcommand_matches("corpus")
            .expect("the corpus subcommand")
            .clone()
    }

    /// The one parse-then-refuse-then-assert ritual every refusal case
    /// below runs: parse `argv` as the CLI would, refuse, and require
    /// exactly `expected`.
    fn assert_refused<S: Into<OsString> + Clone>(argv: &[S], expected: &str) {
        let m = corpus_matches(argv);
        assert_eq!(
            refuse_wrong_lane(workload_of(&m), &m).unwrap_err(),
            expected
        );
    }

    /// The lane groups' own clap declarations are the one home of which
    /// flags are lane-scoped — each group IS the flag set of the lanes
    /// it serves — and the help's lane tags document exactly that:
    /// every flag a group declares carries its group's tag in the
    /// corpus help, and every tagged flag is declared by that group. A
    /// lane-scoped flag added without its tag fails here instead of
    /// silently escaping the documentation. The table the check walks
    /// is [`lane_groups`] itself, never a second hand-maintained one.
    #[test]
    fn lane_scoped_flags_are_tagged_with_their_lane() {
        let corpus = Args::command()
            .find_subcommand("corpus")
            .expect("the corpus subcommand")
            .clone();
        let help_of = |long: &str| -> String {
            corpus
                .get_arguments()
                .find(|arg| arg.get_long() == Some(long))
                .and_then(|arg| arg.get_help())
                .map(|h| h.to_string())
                .unwrap_or_default()
        };
        for group in lane_groups() {
            let mut declared: Vec<String> = group
                .cmd
                .get_arguments()
                .filter_map(|arg| arg.get_long().map(str::to_string))
                .collect();
            declared.sort();
            for long in &declared {
                assert!(
                    help_of(long).contains(&group.tag),
                    "--{long}: a flag of {} must carry the {} tag",
                    group.name,
                    group.tag
                );
            }
            for arg in corpus.get_arguments() {
                let long = arg
                    .get_long()
                    .expect("every corpus argument is a long flag")
                    .to_string();
                let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
                if help.contains(&group.tag) {
                    assert!(
                        declared.contains(&long),
                        "--{long} is tagged {} but not declared by {}",
                        group.tag,
                        group.name
                    );
                }
            }
        }
    }

    /// The wrong-lane refusal itself, both directions derived from the
    /// one table: every flag of every lane group is asserted against
    /// every workload — refused with the group's own name (the same
    /// rendering [`refuse_wrong_lane`] produces) on each workload the
    /// group does not serve, passed on each it does — so a new group,
    /// flag or workload cannot leave a hand-maintained complement list
    /// behind. The sample invocation a flag passes is derived from the
    /// flag's own clap facts: a `SetTrue` flag (`--offline`) passes
    /// itself, a value-taking flag passes its clap default, else its
    /// first possible value, else `1` — the shape every remaining
    /// value-taking flag of the corpus subcommand reads. Passed-ness is
    /// read from real parsed matches — clap's value_source, the same
    /// fact `run()` consults.
    #[test]
    fn wrong_lane_flags_are_refused_naming_the_other_lane() {
        // A clean command line of every workload passes — the graph-benchmark-ldbc-store
        // flags (a passed `--sf` and `--store` among them) on either
        // graph-benchmark-ldbc-store workload, the data-cache flags on every
        // data-fetching workload, included.
        for clean in [
            vec!["corpus", "--workload", "pathdb"],
            vec!["corpus", "--workload", "snb-bi"],
            vec!["corpus", "--workload", "fraud-qpp"],
            vec!["corpus", "--workload", "ukrailway"],
            vec!["corpus", "--workload", "agg-stress"],
            vec![
                "corpus",
                "--workload",
                "snb-bi",
                "--sf",
                "0.1",
                "--store",
                "/tmp/s.rl",
            ],
            vec!["corpus", "--workload", "ukrailway", "--data", "/tmp/cache"],
            vec!["corpus", "--workload", "ukrailway", "--offline"],
            vec![
                "corpus",
                "--workload",
                "agg-stress",
                "--sf",
                "0.1",
                "--store",
                "/tmp/s.rl",
                "--data",
                "/tmp/cache",
            ],
        ] {
            let m = corpus_matches(&clean);
            assert!(
                refuse_wrong_lane(workload_of(&m), &m).is_ok(),
                "{clean:?} is a clean command line"
            );
        }
        for group in lane_groups() {
            for arg in group.cmd.get_arguments() {
                let long = format!(
                    "--{}",
                    arg.get_long()
                        .expect("every lane-group argument is a long flag")
                );
                let mut invocation = vec![long.clone()];
                // A `SetTrue` flag takes no value; every other argument of
                // the lane groups takes exactly one.
                if !matches!(arg.get_action(), &clap::ArgAction::SetTrue) {
                    let value = arg
                        .get_default_values()
                        .first()
                        .map(|v| v.to_string_lossy().into_owned())
                        .or_else(|| {
                            arg.get_possible_values()
                                .first()
                                .map(|v| v.get_name().to_string())
                        })
                        .unwrap_or_else(|| "1".to_string());
                    invocation.push(value);
                }
                for workload in Workload::ALL {
                    let argv: Vec<String> = [
                        "corpus".to_string(),
                        "--workload".to_string(),
                        workload.as_str().to_string(),
                    ]
                    .into_iter()
                    .chain(invocation.iter().cloned())
                    .collect();
                    if group.served.contains(&workload) {
                        let m = corpus_matches(&argv);
                        assert!(
                            refuse_wrong_lane(workload_of(&m), &m).is_ok(),
                            "{argv:?} is a clean command line"
                        );
                    } else {
                        assert_refused(
                            &argv,
                            &format!(
                                "{long} configure(s) {} only, not --workload {workload}",
                                group.name
                            ),
                        );
                    }
                }
            }
        }
    }
}
