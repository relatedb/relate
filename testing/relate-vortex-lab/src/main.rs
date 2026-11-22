//! Round-1 smoke for the vortex property-store evaluation: write one sorted
//! u64 key column as a vortex file through a plain `std::fs::File` (the same
//! sink a `VfsFile` adapter would present), reopen it through an mmap'd
//! `ByteBuffer`, and read it back — proving the zero-decode mmap open path
//! end to end before any bench is built on it.

use std::fs;

use vortex::array::arrays::PrimitiveArray;
use vortex::array::stream::ArrayStreamExt;
use vortex::array::validity::Validity;
use vortex::array::IntoArray;
use vortex::buffer::{Buffer, ByteBuffer};
use vortex::error::VortexResult;
use vortex::file::{OpenOptionsSessionExt, WriteOptionsSessionExt};
use vortex::io::runtime::single::SingleThreadRuntime;
use vortex::io::runtime::BlockingRuntime;
use vortex::io::session::RuntimeSessionExt;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

fn main() -> VortexResult<()> {
    let n: u64 = 1 << 20;
    let rt = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(rt.handle());

    // One sorted unique-key column, the shape the point-seek bench uses.
    let keys: Vec<u64> = (0..n).collect();
    let array = PrimitiveArray::new(Buffer::from(keys), Validity::NonNullable).into_array();
    let dtype = array.dtype().clone();

    let path = std::env::temp_dir().join("relate-vortex-lab-smoke.vortex");
    let out = fs::File::create(&path)?;
    let summary = session
        .write_options()
        .blocking(&rt)
        .write(out, array.to_array_iterator())?;

    // Open = mmap, zero decode: pages fault in on demand from here on.
    let file = fs::File::open(&path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let vfile = session.open_options().open_buffer(ByteBuffer::from(mmap))?;

    let read = rt.block_on(async {
        vfile
            .scan()?
            .into_array_stream()?
            .read_all()
            .await
    })?;

    assert_eq!(read.len(), n as usize, "row count survived the roundtrip");
    assert_eq!(read.dtype(), &dtype, "dtype survived the roundtrip");

    let file_bytes = fs::metadata(&path)?.len();
    let raw_bytes = n * 8;
    println!(
        "smoke ok: {n} rows, {file_bytes} file bytes (summary {}) vs {raw_bytes} raw ({:.1}%)",
        summary.size(),
        100.0 * file_bytes as f64 / raw_bytes as f64,
    );
    Ok(())
}
