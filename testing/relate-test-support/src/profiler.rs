//! The in-process profiler shared by the profiling harnesses (ADR-0244):
//! `pprof-rs` on a CPU-time timer, every thread, plus the stack model in
//! [`crate::stacks`]. Named `profiler` next to the crate's `sampling`
//! module (the platform RSS samplers) so a path says which of the two it
//! is. Lived in query-latency until `bulk-loadtest --sample` needed it too
//! (the lowest shared module is its home — AGENTS.md).

use std::path::Path;

use crate::stacks::{top_text, Stacks};

/// The default sampling frequency: ~997 Hz, off the round numbers so
/// samples don't lock to a timer phase.
pub const DEFAULT_FREQUENCY: i32 = 997;

/// The build's in-process-sampler availability: the one spelling of the
/// cfg pair gating [`sample`] below (`unix` and the `profiler` feature).
/// Use sites consult this constant instead of re-deriving the pair.
pub const IN_PROCESS_SAMPLER: bool = cfg!(all(unix, feature = "profiler"));

/// The one authored refusal when no in-process profiler is built — the
/// stub below returns it, and harness validation maps it into its own
/// error type, so the feature name and remedy text have one home.
pub const UNAVAILABLE: &str = "this build has no in-process profiler (it needs unix \
and the profiler feature of the harness crate); run the binary under an \
external sampler (samply, perf) instead";

/// Refuse `--sample`-shaped flags when no in-process profiler is built.
pub fn require_available() -> std::result::Result<(), String> {
    if IN_PROCESS_SAMPLER {
        Ok(())
    } else {
        Err(UNAVAILABLE.to_string())
    }
}

/// A value computed under the sampler, with what the sampler recorded.
pub struct Sampled<T> {
    pub value: T,
    pub stacks: Stacks,
    pub flamegraph_svg: Vec<u8>,
}

/// Run `f` under `pprof-rs` at `frequency` samples per CPU-second.
///
/// On Linux, libc, libgcc, pthread and the vDSO are blocklisted, as
/// pprof-rs advises (libgcc's unwinder is not signal-safe, and some vDSO
/// DWARF is wrong); samples whose program counter is inside them, including
/// time in glibc's `malloc`, are dropped there. macOS samples everything.
#[cfg(all(unix, feature = "profiler"))]
pub fn sample<T>(frequency: i32, f: impl FnOnce() -> T) -> Result<Sampled<T>, String> {
    use crate::stacks::Frame;

    let mut builder = pprof::ProfilerGuardBuilder::default().frequency(frequency);
    if cfg!(target_os = "linux") {
        builder = builder.blocklist(&["libc", "libgcc", "pthread", "vdso"]);
    }
    let guard = builder
        .build()
        .map_err(|e| format!("starting the sampler: {e}"))?;
    let value = f();
    let report = guard
        .report()
        .build()
        .map_err(|e| format!("building the sample report: {e}"))?;
    drop(guard);
    // pprof keeps each sample's frames leaf first, and each frame's inlined
    // symbols innermost first: reverse both for root first. Its count is
    // signed; a negative one is a broken report, not a zero — fail loudly
    // instead of casting it away.
    let mut pairs = Vec::with_capacity(report.data.len());
    for (frames, n) in report.data.iter() {
        let n = *n;
        if n < 0 {
            return Err(format!("negative sample count {n} from the profiler"));
        }
        let stack = frames
            .frames
            .iter()
            .rev()
            .flat_map(|frame| frame.iter().rev())
            .map(|sym| {
                let file = sym.filename.as_deref().map(|f| f.to_string_lossy());
                Frame::new(&sym.name(), file.as_deref())
            })
            .collect();
        pairs.push((stack, n as u64));
    }
    let stacks = Stacks::from_pairs(pairs);
    let mut flamegraph_svg = Vec::new();
    report
        .flamegraph(&mut flamegraph_svg)
        .map_err(|e| format!("rendering the flame graph: {e}"))?;
    Ok(Sampled {
        value,
        stacks,
        flamegraph_svg,
    })
}

/// Without the sampler (`--sample` refused elsewhere), say so.
#[cfg(not(all(unix, feature = "profiler")))]
pub fn sample<T>(_frequency: i32, _f: impl FnOnce() -> T) -> Result<Sampled<T>, String> {
    Err(UNAVAILABLE.to_string())
}

/// Functions listed in each of `top.txt`'s two function tables — the one
/// cut every harness's `top.txt` renders at.
const TOP_FUNCTIONS: usize = 40;

/// Write the three outputs a sampled run carries under `out_dir`: `top.txt`
/// (the [`crate::stacks::top_text`] rendering), `folded.txt`, and
/// `flame.svg`. The one policy every profiling harness shares — write
/// errors name the path. Lives here, beside the `Sampled` values it
/// renders, not in the stack model (which never produces the svg).
pub fn write_stack_outputs(
    out_dir: &Path,
    stacks: &Stacks,
    flamegraph_svg: &[u8],
    header: &[String],
) -> std::result::Result<(), String> {
    write_named(
        out_dir,
        "top.txt",
        top_text(stacks, header, TOP_FUNCTIONS).as_bytes(),
    )?;
    write_named(out_dir, "folded.txt", stacks.to_folded().as_bytes())?;
    write_named(out_dir, "flame.svg", flamegraph_svg)
}

/// Write `name` under `out_dir`, naming the path in the error — the one
/// spelling of a harness output write, shared by the stack outputs above
/// and every harness's `summary.json`.
pub fn write_named(out_dir: &Path, name: &str, bytes: &[u8]) -> std::result::Result<(), String> {
    let p = out_dir.join(name);
    std::fs::write(&p, bytes).map_err(|e| format!("writing {}: {e}", p.display()))
}
