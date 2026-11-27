//! Round-1 harness for the vortex property-store evaluation.
//!
//! Kernels are named O1..O20 in `workload-mapping-findings.md`; each reports
//! ns/op, allocs/op, B/op with a load stamp. Measurements are serialized —
//! one kernel at a time on a shared box (quiet-box protocol).

use std::path::PathBuf;

use relate_vortex_lab::{bench, ops, store};

type CmdResult = Result<(), Box<dyn std::error::Error>>;

/// One command's inputs: the resolved paths, the raw argv and the kernel's
/// iteration budget, so a command reads its own extra arguments instead of
/// the dispatcher re-spelling them.
struct Ctx {
    csv: PathBuf,
    out_dir: PathBuf,
    argv: Vec<String>,
    /// The kernel's smoke iteration budget, from its `COMMANDS` row.
    iters: u64,
}

impl Ctx {
    /// The argv token at `index` (0 = the binary, 1 = the verb), parsed.
    /// `None` only when the token is absent; a malformed value is an error
    /// naming it, never a silent default.
    fn parsed<T: std::str::FromStr>(&self, index: usize, what: &str) -> Result<Option<T>, String>
    where
        T::Err: std::fmt::Display,
    {
        match self.argv.get(index) {
            None => Ok(None),
            Some(raw) => raw
                .parse()
                .map(Some)
                .map_err(|e| format!("{what} {raw:?}: {e}")),
        }
    }
}

/// One command: its name, its kernel's smoke iteration budget (0 for a
/// kernel that runs no measured loop) and its runner — the budget stated
/// once, here, never re-spelled in the runner.
struct Command {
    name: &'static str,
    iters: u64,
    run: fn(&Ctx) -> CmdResult,
}

/// The command table — the one list `all` iterates, so the default command
/// can never skip a named kernel. One row per line: the table reads as a
/// table.
#[rustfmt::skip]
const COMMANDS: &[Command] = &[
    Command { name: "point", iters: 100_000, run: point_cmd },
    Command { name: "seek", iters: 20_000, run: seek_cmd },
    Command { name: "filter", iters: 20_000, run: filter_cmd },
    Command { name: "agg", iters: 20_000, run: agg_cmd },
    Command { name: "join", iters: 20_000, run: join_cmd },
    Command { name: "sets", iters: 20_000, run: sets_cmd },
    Command { name: "vfs", iters: 20_000, run: vfs_cmd },
    Command { name: "probe", iters: 2_000, run: probe_cmd },
    Command { name: "seam", iters: 20_000, run: seam_cmd },
    Command { name: "memory", iters: 20_000, run: memory_cmd },
    Command { name: "concurrency", iters: 100_000, run: concurrency_cmd },
    Command { name: "pokec", iters: 20_000, run: pokec_cmd },
    Command { name: "size", iters: 0, run: size_cmd },
];

// The measuring binary's one counting allocator (the kernels read its
// counters through `bench`): a bin chooses, the library never does.
#[global_allocator]
static ALLOC: bench::Counting = bench::Counting;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().collect();
    let cmd = argv.get(1).cloned().unwrap_or_else(|| "all".to_string());
    let out_dir = store::lab_out();
    std::fs::create_dir_all(&out_dir)?;
    let csv = store::lab_csv();
    let ctx = |row: &Command| Ctx {
        csv: csv.clone(),
        out_dir: out_dir.clone(),
        argv: argv.clone(),
        iters: row.iters,
    };

    println!(
        "vortex-lab: out={} load={:.1}",
        out_dir.display(),
        bench::loadavg1()
    );

    if cmd == "all" {
        for row in COMMANDS {
            (row.run)(&ctx(row))?;
        }
        return Ok(());
    }
    let Some(row) = COMMANDS.iter().find(|row| row.name == cmd.as_str()) else {
        let names: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
        eprintln!("unknown command {cmd}: {} | all", names.join(" | "));
        std::process::exit(2);
    };
    (row.run)(&ctx(row))
}

fn size_cmd(ctx: &Ctx) -> CmdResult {
    let base = ctx.csv.parent().ok_or("csv has no parent dir")?;
    ops::point::size_ldbc(base, &ctx.out_dir)
}

fn probe_cmd(ctx: &Ctx) -> CmdResult {
    ops::probe::run(&ctx.csv, &ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn point_cmd(ctx: &Ctx) -> CmdResult {
    ops::point::run(&ctx.csv, &ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn seek_cmd(ctx: &Ctx) -> CmdResult {
    let n = ctx.parsed(2, "seek row count")?.unwrap_or(150_000);
    ops::seek::run("flat", n, 4, &ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn filter_cmd(ctx: &Ctx) -> CmdResult {
    ops::filter::run(&ctx.csv, &ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn agg_cmd(ctx: &Ctx) -> CmdResult {
    ops::agg::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn join_cmd(ctx: &Ctx) -> CmdResult {
    ops::join::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn sets_cmd(ctx: &Ctx) -> CmdResult {
    ops::sets::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn vfs_cmd(ctx: &Ctx) -> CmdResult {
    ops::vfs::run(&ctx.csv, &ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn seam_cmd(ctx: &Ctx) -> CmdResult {
    ops::seam_bench::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn memory_cmd(ctx: &Ctx) -> CmdResult {
    ops::memory::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn concurrency_cmd(ctx: &Ctx) -> CmdResult {
    ops::concurrency::run(&ctx.out_dir, ctx.iters)?;
    Ok(())
}

fn pokec_cmd(ctx: &Ctx) -> CmdResult {
    ops::memory::pokec(&ctx.out_dir, ctx.iters)?;
    Ok(())
}
