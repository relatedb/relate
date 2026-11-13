//! The QPP/LDBC corpus lane (ADR-0479, LABS-646): the translated PathDB
//! RPQ workload, the in-house fraud QPP suite, the translated LDBC SNB BI
//! analytics workload, the verified UK railway graph workload, the
//! in-house agg-stress aggregation workload, and the in-house equi-join
//! workload — the first, third, fifth and sixth executed beside the
//! graph-benchmark-ldbc lane over the same store machinery, the second
//! over generated and fixture graphs, the fourth over its own imported
//! graph.
//!
//! What this module tree owns, in the order a run uses them:
//!
//! - [`Workload`] names a directory under `workloads/`, its
//!   sub-directories ([`Workload::query_dirs`]) and what it runs on (the
//!   graph-benchmark-ldbc store for `pathdb`, `snb-bi`, `agg-stress` and `equi-join`,
//!   generated or fixture graphs for `fraud-qpp` — whose oracle and checks live in
//!   `fraud` — and the workload's own imported graph for `ukrailway`);
//! - this root module owns the query-file side of the format:
//!   [`load_queries`] reads a workload's query files, classifying each
//!   by its `// status:` marker — the marker is the ok-set ratchet: an
//!   `ok` query that stops being ok fails the run, and the ok-set only
//!   shrinks with a tracked reason — and [`Lane`]/[`CorpusConfig`] shape
//!   a run's knobs so each workload carries exactly its own, while the
//!   `files` submodule owns the workload-JSON side (the file-name
//!   conventions and entry parsers every lane's side-table loader reads
//!   through — a params table, a seed table, an oracle file — plus the
//!   rows-oracle machinery the rows-oracle lanes share whole: the
//!   loader and the build-time contract) and `graph` the rows-oracle
//!   route those lanes resolve their entries through;
//! - each lane owns its own construction and routing policy —
//!   `pathdb::PathdbLane` (the graph-benchmark-ldbc store, its seed table
//!   `pathdb::seeds::SeedTable`, the run-vs-plan rule over the seed
//!   table), `fraud::FraudGraphs` (the oracle, the graphs it routes
//!   onto), `snbbi::SnbBiLane` (the graph-benchmark-ldbc store, its recorded parameter
//!   table, the run-vs-plan rule over it), `ukrailway::UkrailwayLane`
//!   (the fetched data set, the import script's `:memory:` graph, the
//!   verified rows oracle) and `oracle_store::RowsOracleStoreLane` (the
//!   graph-benchmark-ldbc store, the per-scale-factor verified rows
//!   oracle — the ONE lane every in-house scale-table rows-oracle
//!   workload builds (`new` takes the workload and derives its one
//!   oracle path; ukrailway's one-size data set keeps its own lane and
//!   its single `oracle.json`):
//!   agg-stress, LABS-733's measuring net for the planned chunk-native
//!   streaming fold; equi-join, LABS-1531's measuring net for the
//!   planned hash-join operator) — so the runner's lane match is a
//!   one-line delegation per arm;
//! - `runner::run_corpus` executes every entry under the watchdog —
//!   `ok` runs and is answer-checked where an oracle exists, `kept` is
//!   parse- and EXPLAIN-checked but never executed (frontier-NFA
//!   acceptance cases, LABS-644), `error-pin` must fail with its
//!   authored error — and [`CorpusReport`] is the JSON report and the
//!   console table.
//!
//! Nothing here judges latency: a corpus run reports timings but gates
//! only on the ratchet and the pins (ADR-0289 memory-shape precedent).
//!
//! # Visibility ladder
//!
//! One ladder for the whole subtree, stated here once: items are
//! private by default; a sibling corpus module's need is the only
//! promotion, and since every submodule below is private to `corpus`,
//! that promotion is `pub(super)` — from a submodule, visible exactly
//! inside the corpus tree — except a crate-level module's need
//! (`fetch.rs`'s ukrailway sha pin reading [`Workload::dir`]), which
//! promotes one step further to `pub(crate)`; the only `pub` items are
//! this root's own
//! vocabulary plus its re-exports, which together are what code outside
//! the crate imports (the `query-latency` binary and `tests/corpus.rs`):
//! [`Workload`], [`Classification`], [`CorpusConfig`], [`Lane`],
//! [`SeedPolicy`], [`workload_queries`], [`Lane::default_timeout`],
//! [`Lane::report_name`], [`run_corpus`], [`CorpusReport`]
//! (with the report types it exposes), [`fraud_oracle`] and
//! [`FraudCheck`]. The shared execution types (`GraphHandle`-kind
//! state, the entry target, the run's dispositions) and the lane
//! interface every lane module implements (`CorpusLane`) live in
//! `graph`, and the workload-JSON file-format helpers in `files`, so
//! each lane
//! depends only on the corpus tree's shared modules: `runner → fraud`,
//! `runner → pathdb`, `runner → snbbi`, `runner → ukrailway` and
//! `runner → oracle_store` (the rows-oracle lanes) are the edges into
//! the lanes, never lane → lane.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use relate_datasets::fraud::FraudConfig;
use relate_test_support::hex;
use serde::Serialize;
use sha2::{Digest, Sha256};

mod aggstress;
mod equijoin;
mod files;
#[cfg(test)]
mod fixtures;
mod fraud;
mod graph;
mod oracle_store;
mod pathdb;
mod report;
mod runner;
mod snbbi;
mod ukrailway;

pub use fraud::{fraud_oracle, FraudCheck};
pub use report::CorpusReport;
pub use runner::run_corpus;

use crate::suite::GraphOptions;
use crate::workload::ScaleFactor;
use crate::{read, split_header, Result};

/// The ukrailway lane's own default per-entry timeout: its r02 entry is
/// minutes-scale over the imported graph (the workload README records the
/// measured time), so the lane carries its own bound — the workload runs
/// at the data set's one size, so there is no scale factor to scale the
/// per-entry default with, and the 10 s default would fail the ratchet on
/// day one. A timeout is still a ratchet failure for an `ok` entry: the
/// bound is generous, not absent.
const UKRAILWAY_TIMEOUT: Duration = Duration::from_secs(600);

/// The fraud lane's flat per-entry default: the fraud graphs are
/// generator-sized — the lane runs no scale factor, so there is nothing
/// to scale the default with, and the SF0.1 bound is simply fixed.
const FRAUD_TIMEOUT: Duration = Duration::from_secs(10);

/// The agg-stress lane's own flat per-entry default (LABS-733): its
/// slowest entry — a02, the composite-key fold over `knows*1..3` —
/// measures 15.3 s over the graph-benchmark-ldbc SF0.1 store and the a01 headline 10.3 s
/// (the workload README records the measurements), both past the
/// graph-benchmark-ldbc-store lanes' 10 s SF0.1 default, so the lane carries its own
/// bound at ~4x the slowest measured entry — generous rather than tight,
/// because a corpus lane reports timings and never gates on them; the
/// bound only has to catch an unbounded walk, not a 20% regression.
/// Flat, not scale-factor-scaled, because the workload is recorded at
/// SF0.1 only — an SF1 run fails loudly at its oracle load (no
/// `oracle-sf1.json` exists), so the bound serves exactly the one
/// recorded scale. A timeout is still a ratchet failure for an `ok`
/// entry: the bound is generous, not absent.
const AGG_STRESS_TIMEOUT: Duration = Duration::from_secs(60);

/// The equi-join lane's own flat per-entry default (LABS-1531): its
/// slowest entry — e03, the cross-product headline — measures 7.2 s over
/// the graph-benchmark-ldbc SF0.1 store on a loaded machine, inside the
/// graph-benchmark-ldbc-store lanes' 10 s SF0.1 default but
/// product-dominated and therefore machine-sensitive — the workload
/// README's measured ladder has the same shape's next bound at 59.6 s
/// (the unbounded form 102 s), and a 2-vCPU-capped runner moves a
/// product entry several-fold. The lane carries its own generous bound
/// at ~4x the slowest recorded entry, because a corpus lane reports
/// timings and never gates on them; the bound only has to catch an
/// unbounded product, not a join rewrite's regression. Flat, not
/// scale-factor-scaled, because the workload is recorded at SF0.1 only —
/// an SF1 run fails loudly at its oracle load (no `oracle-sf1.json`
/// exists), so the bound serves exactly the one recorded scale. A
/// timeout is still a ratchet failure for an `ok` entry: the bound is
/// generous, not absent.
const EQUI_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

/// The workload vocabulary, spelled ONCE: the enum, its `ALL` list and
/// its directory names are all emitted from this table, so adding a
/// workload cannot leave a hand-maintained `ALL` behind (the enum's
/// other exhaustive matches fail to compile on a new variant; a plain
/// `ALL` const would not).
macro_rules! spell_workloads {
    ($($variant:ident ($($doc:literal)*) = $name:literal;)*) => {
        /// Which corpus workload a run executes.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Workload {
            $(
                $(#[doc = $doc])*
                $variant,
            )*
        }

        impl Workload {
            /// Every workload, derived from the same spelling as the
            /// variants — never a second table to keep in step.
            pub const ALL: [Workload; [$($name),*].len()] = [$(Self::$variant),*];

            /// The workload's name — its directory under `workloads/`
            /// and the `--workload` flag's value.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }

        impl std::str::FromStr for Workload {
            type Err = String;

            fn from_str(s: &str) -> Result<Self> {
                for w in Self::ALL {
                    if s == w.as_str() {
                        return Ok(w);
                    }
                }
                let names: Vec<&str> = Self::ALL.iter().map(|w| w.as_str()).collect();
                Err(format!("corpus workload is one of {names:?}, got {s:?}"))
            }
        }
    };
}

spell_workloads! {
    Pathdb(
        "The 142 translated PathDB RPQs plus the 65-restrictor matrix,"
        " over the graph-benchmark-ldbc store (the same data the `run` lane uses)."
    ) = "pathdb";
    FraudQpp(
        "The six in-house fraud QPP queries over generated and fixture"
        " graphs (no external data)."
    ) = "fraud-qpp";
    SnbBi(
        "The 16 translated LDBC SNB BI analytics queries over the graph-benchmark-ldbc"
        " store (upstream defaults as substitution parameters)."
    ) = "snb-bi";
    Ukrailway(
        "The two verified UK railway graph queries over the workload's"
        " own imported graph (fetched dogofbrian/ukrailway data, no graph-benchmark-ldbc"
        " store)."
    ) = "ukrailway";
    AggStress(
        "The five in-house aggregation-stress queries over the graph-benchmark-ldbc store"
        " (LABS-733: result streams dominated by rows flowing into a"
        " fold, the measuring net for the planned chunk-native streaming"
        " fold)."
    ) = "agg-stress";
    EquiJoin(
        "The three in-house equi-join queries over the graph-benchmark-ldbc store"
        " (LABS-1531: two patterns sharing no anchor joined by a WHERE"
        " equality, the measuring net for the planned hash-join"
        " operator)."
    ) = "equi-join";
}

impl Workload {
    /// `workloads/<name>/` in this crate — under the crate-wide
    /// [`crate::workloads_dir`] root, the one derivation of it.
    pub(crate) fn dir(self) -> PathBuf {
        crate::workloads_dir().join(self.as_str())
    }

    /// This workload's rows-oracle file, or `None` when it has none —
    /// the ONE derivation every site consults (each rows-oracle lane's
    /// loader, its build-time validation, its route's miss text and the
    /// tests), so no call site re-spells the convention and no workload
    /// is told a file that does not exist. Both conventions are stated
    /// here: the one-size data sets (ukrailway, fraud-qpp) keep a single
    /// `workloads/<name>/oracle.json`, while the two scale-table
    /// workloads resolve `workloads/<name>/oracle-sf<SF>.json`. pathdb
    /// and snb-bi answer against recorded params and seeds rather than a
    /// rows table, so they have no oracle at all.
    pub(crate) fn oracle_path(self, sf: ScaleFactor) -> Option<PathBuf> {
        match self {
            Self::Ukrailway | Self::FraudQpp => Some(self.dir().join("oracle.json")),
            Self::AggStress | Self::EquiJoin => Some(self.scale_table("oracle", sf)),
            Self::Pathdb | Self::SnbBi => None,
        }
    }

    /// A workload's per-scale-factor side-table path —
    /// `workloads/<name>/<stem>-sf<SF>.json` — the ONE derivation of
    /// "this workload, this stem" (the agg-stress oracle and the
    /// params tables), so a lane never re-spells the
    /// wrapper its workload already names.
    pub(crate) fn scale_table(self, stem: &str, sf: ScaleFactor) -> PathBuf {
        files::scale_file(&self.dir(), stem, sf)
    }

    /// The sub-directories holding this workload's query files, in load
    /// order — the loader ([`workload_queries`]) iterates exactly this
    /// list, so which workload owns a second directory (pathdb's
    /// restrictor matrix) is a fact of the enum, not of the loader.
    pub(crate) fn query_dirs(self) -> &'static [&'static str] {
        match self {
            Self::Pathdb => &["queries", "restrictors"],
            Self::FraudQpp | Self::SnbBi | Self::Ukrailway | Self::AggStress | Self::EquiJoin => {
                &["queries"]
            }
        }
    }
}

impl std::fmt::Display for Workload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a query file is treated by a run. Parsed from the file's
/// `// status:` header marker — the one machine-read line of the file
/// format; the rest of the header is provenance prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Execute and require success: the ok-set ratchet. A query marked
    /// `ok` that times out, errors or (where an oracle exists) answers
    /// wrongly fails the run; moving a query out of `ok` needs a tracked
    /// reason, like the TCK allowlist.
    Ok,
    /// Parse and EXPLAIN only, never execute: the frontier-NFA
    /// acceptance cases (LABS-644) the engine cannot finish today.
    Kept,
    /// Execute and require the named authored error (the fail-closed
    /// `UnsafeRepeatableElements` contract pins).
    ErrorPin { error: String },
}

impl Classification {
    /// The `// status:` marker's grammar: `ok <prose>`, `kept <prose>`
    /// or `error-pin <ErrorName> <prose>` (the error name is the
    /// contract the engine error must name).
    fn parse_status(status: &str) -> Result<Self> {
        let (word, rest) = status
            .split_once(' ')
            .ok_or_else(|| format!("status {status:?} has no prose after its marker word"))?;
        match word {
            "ok" => Ok(Self::Ok),
            "kept" => Ok(Self::Kept),
            "error-pin" => {
                let (error, _) = rest
                    .split_once(' ')
                    .ok_or_else(|| format!("error-pin {rest:?} names no error"))?;
                if !error.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(format!(
                        "error-pin names {error:?}; the pin is an authored error name"
                    ));
                }
                Ok(Self::ErrorPin {
                    error: error.to_string(),
                })
            }
            other => Err(format!(
                "status marker is ok, kept or error-pin, got {other:?}"
            )),
        }
    }

    /// The classification's one word — the report's JSON value, the
    /// console table and the `// status:` marker's first token.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Kept => "kept",
            Self::ErrorPin { .. } => "error-pin",
        }
    }
}

/// Whether an engine error message meets an `error-pin` marker's
/// contract: the authored error name must START the engine's rendered
/// parse error — `syntax error: parse error: <AuthoredName>: …` — the
/// same prefix convention the parser's own contract test pins, never
/// the name merely appearing somewhere in a message. One spelling of
/// the contract, shared by the runner and the corpus test net.
pub fn pin_met(error: &str, message: &str) -> bool {
    message
        .split_once("parse error: ")
        .is_some_and(|(_, authored)| authored.starts_with(&format!("{error}:")))
}

impl Serialize for Classification {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// One query file of a corpus workload.
#[derive(Debug, Clone)]
pub struct CorpusQuery {
    pub id: String,
    pub classification: Classification,
    /// The one statement that runs, parameters unresolved.
    pub text: String,
    /// sha256 of `text`, so a report names exactly what ran.
    pub sha256: String,
}

/// Read every `*.cypher` file of `dir/<sub>/` as a corpus query, in file
/// name order. Every file must carry exactly one `// status:` marker. A
/// directory entry that cannot be read fails the run — a vanished
/// corpus file must not weaken the ratchet silently.
fn load_queries(dir: &Path, sub: &str) -> Result<Vec<CorpusQuery>> {
    let sub_dir = dir.join(sub);
    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(&sub_dir).map_err(|e| format!("reading {}: {e}", sub_dir.display()))?
    {
        let path = entry
            .map_err(|e| format!("reading {}: {e}", sub_dir.display()))?
            .path();
        if path.extension().is_some_and(|x| x == "cypher") {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(format!("{} holds no .cypher files", sub_dir.display()));
    }
    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("{}: not a UTF-8 file name", path.display()))?
            .to_string();
        let text = read(&path)?;
        // The header is the file's provenance prose; only its status
        // marker is machine-read, so only the marker leaves this scope.
        let (header, body) = split_header(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let statuses: Vec<&str> = header
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("// status:"))
            .collect();
        let classification = match statuses.as_slice() {
            [one] => Classification::parse_status(one.trim())
                .map_err(|e| format!("{}: {e}", path.display())),
            [] => Err(format!(
                "{}: no `// status:` marker (ok, kept or error-pin)",
                path.display()
            )),
            [..] => Err(format!(
                "{}: {} `// status:` markers",
                path.display(),
                statuses.len()
            )),
        }?;
        let sha256 = hex(&Sha256::digest(body.as_bytes()));
        out.push(CorpusQuery {
            id,
            classification,
            text: body,
            sha256,
        });
    }
    Ok(out)
}

/// A workload's queries: every file of its [`Workload::query_dirs`]
/// sub-directories, in order, with ids required distinct across them.
pub fn workload_queries(workload: Workload) -> Result<Vec<CorpusQuery>> {
    let dir = workload.dir();
    let mut queries = Vec::new();
    for sub in workload.query_dirs() {
        queries.extend(load_queries(&dir, sub)?);
    }
    let distinct = BTreeSet::<&String>::from_iter(queries.iter().map(|q| &q.id));
    if distinct.len() != queries.len() {
        return Err(format!(
            "the {workload} workload has duplicate query ids across its directories"
        ));
    }
    Ok(queries)
}

/// The rows-oracle build sequence every lane runs: derive the workload's
/// oracle path ([`Workload::oracle_path`], the one home of the
/// convention), load the table and validate the workload against it. One
/// home, so no lane — and no test lane — can derive, load or validate
/// differently from another; a workload that carries no oracle file
/// fails loudly here rather than at the call site. Stated in this root —
/// beside [`Workload::oracle_path`], the derivation it composes first —
/// because it is root policy over the `files` submodule's format
/// surface, never a piece of that leaf.
fn load_oracle(
    workload: Workload,
    sf: ScaleFactor,
    queries: &[CorpusQuery],
) -> Result<(files::Oracle, PathBuf)> {
    let path = workload
        .oracle_path(sf)
        .ok_or_else(|| format!("the {workload} workload carries no rows oracle"))?;
    let oracle = files::load_rows_oracle(&path)?;
    files::validate_rows_oracle(&oracle, queries, &path)?;
    Ok((oracle, path))
}

/// How the run picks its seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedPolicy {
    /// Re-resolve every seed key with the degree-median policy on the
    /// loaded store and report every difference from the recorded seed
    /// (the regeneration rule: ids are datagen outputs, so a re-frozen
    /// data set gets new seeds — an expected diff, never a failure).
    Median,
    /// Run with the seeds recorded in `params-sf<SF>.json`, resolving
    /// nothing (the fast, pinned mode).
    Recorded,
}

impl std::str::FromStr for SeedPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "median" => Ok(Self::Median),
            "recorded" => Ok(Self::Recorded),
            other => Err(format!("seed policy is median or recorded, got {other:?}")),
        }
    }
}

impl SeedPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Median => "median",
            Self::Recorded => "recorded",
        }
    }
}

/// The lane-specific setup of a run: exactly the knobs its workload
/// uses, so a run matches its workload once and every knob has one
/// home — each variant's own doc carries its payload's why (which
/// knobs are the lane's own and which are the workload's recorded
/// tables), never shared config a lane would record without running;
/// the variants are the list, no prose re-counts them.
#[derive(Debug, Clone)]
pub enum Lane {
    /// The PathDB RPQs over the graph-benchmark-ldbc store.
    Pathdb {
        /// The graph-benchmark-ldbc graph options — the same store machinery as the
        /// `run` lane, carrying the run's scale factor (the one home
        /// of that fact).
        graph: GraphOptions,
        /// How the run picks its seeds.
        seed_policy: SeedPolicy,
    },
    /// The in-house fraud QPP suite over generated and fixture graphs.
    Fraud {
        /// The fraud generator's knobs.
        fraud: FraudConfig,
    },
    /// The LDBC SNB BI analytics queries over the graph-benchmark-ldbc store.
    SnbBi {
        /// The graph-benchmark-ldbc graph options — the same store machinery as the
        /// `run` lane, carrying the run's scale factor.
        graph: GraphOptions,
    },
    /// The UK railway graph queries over the workload's own imported
    /// graph.
    Ukrailway {
        /// The data cache the fetched CSVs live under.
        data_root: PathBuf,
        /// Whether a missing or mismatched CSV may be downloaded.
        download: bool,
    },
    /// The in-house aggregation-stress queries over the graph-benchmark-ldbc store
    /// (LABS-733).
    AggStress {
        /// The graph-benchmark-ldbc graph options — the same store machinery as the
        /// `run` lane, carrying the run's scale factor (the one home
        /// of that fact; the workload is recorded at SF0.1 only).
        graph: GraphOptions,
    },
    /// The in-house equi-join queries over the graph-benchmark-ldbc store
    /// (LABS-1531).
    EquiJoin {
        /// The graph-benchmark-ldbc graph options — the same store machinery as the
        /// `run` lane, carrying the run's scale factor (the one home
        /// of that fact; the workload is recorded at SF0.1 only).
        graph: GraphOptions,
    },
}

impl Lane {
    /// The workload this lane runs — derived, never stated twice.
    pub fn workload(&self) -> Workload {
        match self {
            Self::Pathdb { .. } => Workload::Pathdb,
            Self::Fraud { .. } => Workload::FraudQpp,
            Self::SnbBi { .. } => Workload::SnbBi,
            Self::Ukrailway { .. } => Workload::Ukrailway,
            Self::AggStress { .. } => Workload::AggStress,
            Self::EquiJoin { .. } => Workload::EquiJoin,
        }
    }

    /// The lane-scoped facts a run records, derived from the lane's own
    /// knobs — a fact a lane cannot run is never reported: the graph-benchmark-ldbc-store
    /// lanes record the scale factor their tables ran at, the pathdb
    /// lane its seed policy too; a fraud run records neither, its
    /// generator knobs being the fraud block's own, and a ukrailway run
    /// neither — its graph runs at the data set's one size, with no
    /// seed to resolve.
    fn report_facts(&self) -> (Option<ScaleFactor>, Option<SeedPolicy>) {
        match self {
            Self::Pathdb { graph, seed_policy } => (Some(graph.sf), Some(*seed_policy)),
            Self::SnbBi { graph } | Self::AggStress { graph } | Self::EquiJoin { graph } => {
                (Some(graph.sf), None)
            }
            Self::Fraud { .. } | Self::Ukrailway { .. } => (None, None),
        }
    }

    /// The per-entry timeout a run of this lane defaults to (ADR-0479),
    /// each arm reading its own facts. The two table-recorded lanes
    /// that run at several scale factors — pathdb and snb-bi — scale it
    /// off their own graph scale factor (10 s at SF0.1, 20 s at SF1,
    /// the one home of that fact being the lane's graph options). Every
    /// other lane carries its own flat bound, each const's doc holding
    /// its why (the arms below are the list, no prose re-counts them).
    /// A timeout is a recorded outcome for a `kept` entry's
    /// history, and a failure for an `ok` one — the ratchet.
    pub fn default_timeout(&self) -> Duration {
        match self {
            Self::Pathdb { graph, .. } | Self::SnbBi { graph, .. } => match graph.sf {
                ScaleFactor::Sf0_1 => Duration::from_secs(10),
                ScaleFactor::Sf1 => Duration::from_secs(20),
            },
            Self::Fraud { .. } => FRAUD_TIMEOUT,
            Self::Ukrailway { .. } => UKRAILWAY_TIMEOUT,
            Self::AggStress { .. } => AGG_STRESS_TIMEOUT,
            Self::EquiJoin { .. } => EQUI_JOIN_TIMEOUT,
        }
    }

    /// The default report file name of a run of this lane:
    /// `report-corpus-<name>[-sf<SF>].json` — the name derived from the
    /// lane's own [`Workload::as_str`], never re-spelled per variant,
    /// with the `-sf<SF>` segment derived from [`Lane::report_facts`]:
    /// the lanes that record a scale factor carry it in their report
    /// name, the one-size lanes (fraud-qpp's generator knobs, ukrailway's
    /// single-size data set) record none and name their report without
    /// it — one fact, consulted, never re-matched.
    pub fn report_name(&self) -> String {
        match self.report_facts().0 {
            Some(sf) => format!("report-corpus-{}-sf{sf}.json", self.workload().as_str()),
            None => format!("report-corpus-{}.json", self.workload().as_str()),
        }
    }
}

/// The repeat threshold: a successful entry faster than this repeats
/// for the min/median columns; a slower one is executed once, like the
/// spike driver.
const REPEAT_LIMIT: Duration = Duration::from_millis(1500);

/// How a corpus run is configured. Every lane-scoped knob lives on the
/// [`Lane`] — the shared fields are the ones every lane runs under.
#[derive(Debug, Clone)]
pub struct CorpusConfig {
    /// The lane: which workload runs, with its lane-specific knobs.
    pub lane: Lane,
    pub timeout: Duration,
    /// Extra timed executions of cheap queries, for min/median (a query
    /// slower than `REPEAT_LIMIT` is executed once, like the spike
    /// driver).
    pub repeats: usize,
}

#[cfg(test)]
mod tests;
