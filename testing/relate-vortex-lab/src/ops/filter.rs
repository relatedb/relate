//! filter kernels — see workload-mapping-findings.md for the spec.
//! Implemented by a fan-out agent; signature fixed by main.rs.

use std::path::Path;

use crate::bench::Measured;

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let _ = (out_dir, iters);
    Err("filter: not implemented yet".into())
}
