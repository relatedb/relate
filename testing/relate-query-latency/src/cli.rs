//! The pokec profiling harness's clap surface: the flags shared by the
//! `pokec_profile` bin and the Vortex A/B driver's `pokec` subcommand. The
//! library entry ([`crate::pokec::run_groups`]) takes the typed
//! [`crate::pokec::Options`] these flags resolve to.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use relate_test_support::pokec::PokecShape;

use crate::pokec::{group_names, Mode, Options, Timed};

/// The fixture shape to open.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Shape {
    /// 10,000 users / 121,716 friendships.
    Small,
    /// 100,000 users / 1,768,164 friendships.
    Medium,
}

impl Shape {
    fn fixture(self) -> PokecShape {
        match self {
            Shape::Small => PokecShape::SMALL,
            Shape::Medium => PokecShape::MEDIUM,
        }
    }
}

/// The pokec harness's arguments, flattened by each binary's own parser.
#[derive(Debug, Args)]
pub struct PokecArgs {
    /// Which pokec shape to open.
    #[arg(value_enum)]
    shape: Shape,
    /// Run only this group; every group when omitted. The valid names are
    /// the listed possible values (the group catalogue is the one source).
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(group_names()))]
    group: Option<String>,
    /// Run the selected groups under the in-process sampler and write
    /// `<DIR>/<group>/top.txt`, `folded.txt` and `flame.svg`.
    #[arg(long, value_name = "DIR", conflicts_with = "profile")]
    sample: Option<PathBuf>,
    /// Print the `PROFILE` result of the group's query at start id 1
    /// instead of timing it.
    #[arg(long, requires = "group")]
    profile: bool,
    /// Time every round as `PROFILE <query>` instead of the bare query: the
    /// profiled arm of the memory A/B (ADR-0654).
    #[arg(long, conflicts_with = "profile")]
    profiled: bool,
}

impl PokecArgs {
    /// The typed run this flag set resolves to.
    pub fn into_options(self) -> Options {
        Options {
            shape: self.shape.fixture(),
            group: self.group,
            mode: if self.profile {
                Mode::Profile
            } else {
                Mode::Timed(Timed {
                    profiled: self.profiled,
                    sample_dir: self.sample,
                })
            },
        }
    }
}
