# Common brief — vortex lab operator kernels (REL8-1982 round 1)

## Where
- Worktree: `/Users/mh/d/rust/relate-wt/vortex-lab`, branch `perf/rel8-1982-vortex-lab`.
- Crate: `testing/relate-vortex-lab` — a measurement lab, never shipped, MSRV lifted to 1.95 locally.
- Check: `cd /Users/mh/d/rust/relate-wt/vortex-lab && CARGO_TARGET_DIR=/Users/mh/d/rust/relate-wt/vortex-lab/target cargo check -p relate-vortex-lab`
- Run: same with `cargo run -q -p relate-vortex-lab -- <cmd>`; for CSV-based kernels set
  `VORTEX_LAB_CSV=/Users/mh/d/rust/relate/target/query-latency/data/social_network-sf0.1-CsvBasic-StringDateFormatter/dynamic/person_0_0.csv`
- Spec of every kernel (O1..O20): `/Users/mh/d/rust/relate-wt/vortex-lab/workload-mapping-findings.md`.

## Ownership — read this twice
You own **exactly one file**, `src/ops/<your-module>.rs`. Do NOT edit `main.rs`, `ops/mod.rs`,
`store.rs`, `bench.rs`, `point.rs`, `seek.rs`, `Cargo.toml` — other agents own those and are editing
them in the same worktree right now. Write your file with a Bash heredoc (`cat > path <<'EOF'`);
the Write tool may be denied in background agents.

Your signature is already wired in `main.rs`:
```rust
pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>>
```

## Shared helpers you call (do not re-implement)
- `crate::store::{Table, Ty, Col, synthetic, load_csv, write, open_mmap}`
  - `synthetic(shape, n, attrs)` — shape is `"flat"` or `"ring"`; columns: `id` (dense i64), `key` (unique string), `attr0..attrN` (i64, repeating distribution).
  - `load_csv(path, &[(&str, Ty)])` — pipe-delimited LDBC CSV.
  - `write(path, &Table) -> (bytes, per_column_bytes)`; `open_mmap(path) -> (session, rt, VortexFile)`.
  - `Table::{rows, raw_bytes, column, to_vortex}`; `Col::{len, raw_bytes}`.
- `crate::bench::{measure, Measured, loadavg1}` — `measure(name, iters, |i| { ... })` returns ns/op,
  allocs/op, B/op and load stamps. **Always use it** so every number carries a load stamp.

## Vortex 0.93 API facts proven in this crate (copy these, do not rediscover)
```rust
use vortex::VortexSessionDefault;
use vortex::session::VortexSession;
use vortex::io::runtime::BlockingRuntime;             // for rt.block_on
use vortex::io::session::RuntimeSessionExt;           // for .with_handle
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::array::stream::ArrayStreamExt;            // read_all
use vortex::array::VortexSessionExecute;              // session.create_execution_ctx()
use vortex::array::arrays::struct_::{Struct, StructArrayExt}; // as_opt::<Struct>() + unmasked_field_by_name
use vortex::buffer::{Buffer, ByteBuffer};
use vortex::scan::strict_sorted_buffer::StrictSortedBuffer;
```
- session: `VortexSession::default().with_handle(rt.handle())`, `rt = SingleThreadRuntime::default()`
- write: `session.write_options().blocking(&rt).write(file, array.to_array_iterator())?` → `WriteSummary`
  (`.size()`, `.compressed_column_sizes()`)
- open: `session.open_options().open_buffer(ByteBuffer::from(unsafe { memmap2::Mmap::map(&f)? }))?`
- scan: `rt.block_on(async { vfile.scan()?.into_array_stream()?.read_all().await })?`
- scan builder: `.with_filter(bound_expr)`, `.with_projection(bound_expr)`,
  `.with_row_indices(StrictSortedBuffer::try_new(Buffer::from(vec))?)`, `.with_row_range(a..b)`,
  `.with_limit(n)`, `.with_selection(sel)`
- struct field: `root.as_opt::<Struct>().ok_or("...")?.unmasked_field_by_name("x")?.clone()`
- one value: `arr.execute_scalar(row, &mut ctx)?` with `let mut ctx = session.create_execution_ctx();`
- file-level: `vfile.can_prune(&filter)?`, `vfile.splits()?`, `vfile.file_stats()`, `vfile.row_count()`
- array stats: `arr.statistics().get_as::<i64>(Stat::Min)` — check the exact path/enum in
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/vortex-array-0.93.0/src/expr/stats/mod.rs`
  (variants: IsConstant, IsSorted, IsStrictSorted, Max, Min, Sum, NullCount, UncompressedSizeInBytes, NaNCount)
- expressions: `vortex::array::expr::{root, lit, ...}` — **read the real API** at
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/vortex-array-0.93.0/src/expr/` before
  writing (functions like `gt`/`eq`/`get_item` exist in some form); bind with `.bind(file.dtype())?`
  then `.optimize_recursive()?`.
- Everything in the vortex source is readable at
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/vortex-*-0.93.0/` — prefer reading it over guessing.

## Rules
1. **Measure only your kernels.** The box is shared and loaded (load ~20): no measurement campaigns, no
   full-suite runs. A single smoke run with small `iters` (≤ 2000) to prove the kernel works is expected
   and welcome; report its numbers with the load stamp.
2. **No new dependencies** (Cargo.toml is shared and owned by the main context).
3. Prefer a kernel that works and reports honestly over one that is clever. If a mechanism you need is
   not available in 0.93, implement the closest honest thing and say so in your report.
4. Return a compact report: the file written, what each kernel measures, the smoke numbers, and any API
   surprise or measurement caveat (that last part matters most).
