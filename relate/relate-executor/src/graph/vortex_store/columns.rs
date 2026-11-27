//! The generic Vortex array plumbing the store's arms share: the one
//! single-column projected read and the field/chunk decomposition of its
//! result — ported from the lab seam with the store (ADR-0665). The
//! sorted side-file's own machinery lives in `side.rs`.

use relate_core::{Error, Result};

use super::vortex_error;
use vortex::array::arrays::struct_::{Struct, StructArrayExt};
use vortex::array::ArrayRef;
use vortex::file::VortexFile;
use vortex::io::runtime::BlockingRuntime;

/// The module's one single-column read: bind the projection, optimize it,
/// scan the file for exactly that column (the lab store's `projected`,
/// ported with the store). A failure is a typed storage error — the
/// caller either propagates it or declines the read to the compact
/// records; nothing here panics on the user read path.
pub(super) fn projected<R: BlockingRuntime>(
    rt: &R,
    vfile: &VortexFile,
    name: &str,
) -> Result<ArrayRef> {
    use vortex::array::stream::ArrayStreamExt;
    use vortex::expr::{col, BoundExpression};
    let projection: BoundExpression = col(name)
        .bind(vfile.dtype())
        .map_err(vortex_error)?
        .optimize_recursive()
        .map_err(vortex_error)?;
    let want = projection.dtype().clone();
    // The scan-with-projection idiom the lab store's `scan` wrapper
    // spells (ported for this module's one single-column read).
    let arr = rt
        .block_on(async {
            let b = vfile.scan()?.with_projection(projection.clone());
            b.into_array_stream()?.read_all().await
        })
        .map_err(vortex_error)?;
    if arr.as_opt::<Struct>().is_none() && *arr.dtype() != want {
        return Err(Error::storage(format!(
            "projected scan of {name} yielded {} instead of the column",
            arr.dtype()
        )));
    }
    field_of(arr, name)
}

/// The one field extraction: a struct array yields its named field (an
/// absent name is a typed storage error); a plain array IS the column and
/// `name` is not consulted.
fn field_of(array: ArrayRef, name: &str) -> Result<ArrayRef> {
    match array.as_opt::<Struct>() {
        Some(view) => Ok(view
            .unmasked_field_by_name(name)
            .map_err(vortex_error)?
            .clone()),
        None => Ok(array),
    }
}

/// The one chunk decomposition of a column array (the lab store's
/// `chunks`, ported).
pub(super) fn chunks(field: &ArrayRef) -> Vec<ArrayRef> {
    use vortex::array::arrays::chunked::{Chunked, ChunkedArrayExt};
    match field.as_opt::<Chunked>() {
        Some(chunked) => (0..chunked.nchunks())
            .map(|i| chunked.chunk(i).clone())
            .collect(),
        None => vec![field.clone()],
    }
}
