//! The Vortex snapshot codec's shard serialization (ADR-0665): one
//! single-column Vortex file per row window, written to a byte buffer and
//! read back the same way. This is the half that links the Vortex crate;
//! relate-storage keeps the layout (the payload manifest, the shard file
//! names, the orphan retirement) and calls back through its shard-codec
//! registry.
//!
//! [`install`] registers the pair, so a store opened after it resolves the
//! `vortex` codec instead of failing closed.

use relate_core::{Error, Result};
use relate_storage::install_shard_codec;

/// Install the Vortex shard encode/decode pair into relate-storage's
/// process-wide registry. Idempotent: the first install wins.
pub fn install() {
    install_shard_codec(encode, decode);
}

/// Encode rows as a single-column Vortex file in memory — the writer's
/// bytes are handed to relate-storage's VFS, so no host file is touched.
fn encode(rows: &[String]) -> Result<Vec<u8>> {
    use vortex::array::arrays::varbin::VarBinArray;
    use vortex::array::IntoArray;
    use vortex::file::WriteOptionsSessionExt;
    use vortex::io::runtime::single::SingleThreadRuntime;
    use vortex::io::runtime::BlockingRuntime;
    use vortex::io::session::RuntimeSessionExt;
    use vortex::session::VortexSession;
    use vortex::VortexSessionDefault;

    let runtime = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(runtime.handle());
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    let array = VarBinArray::from(refs).into_array();
    let mut bytes = Vec::new();
    session
        .write_options()
        .blocking(&runtime)
        .write(&mut bytes, array.to_array_iterator())
        .map_err(vortex_error)?;
    Ok(bytes)
}

/// Decode a single-column Vortex file back to its UTF-8 rows.
fn decode(bytes: &[u8]) -> Result<Vec<String>> {
    use vortex::array::stream::ArrayStreamExt;
    use vortex::array::VortexSessionExecute;
    use vortex::buffer::ByteBuffer;
    use vortex::file::OpenOptionsSessionExt;
    use vortex::io::runtime::single::SingleThreadRuntime;
    use vortex::io::runtime::BlockingRuntime;
    use vortex::io::session::RuntimeSessionExt;
    use vortex::scalar::ScalarValue;
    use vortex::session::VortexSession;
    use vortex::VortexSessionDefault;

    if bytes.is_empty() {
        // Every shard `encode` writes carries the file framing, zero rows
        // included — zero bytes on disk is a torn write, not a shard of
        // zero rows. Fail the load instead of silently answering a short
        // graph.
        return Err(vortex_error(
            "an empty shard is a torn write, not a zero-row shard",
        ));
    }
    let runtime = SingleThreadRuntime::default();
    let session = VortexSession::default().with_handle(runtime.handle());
    let file = session
        .open_options()
        .open_buffer(ByteBuffer::copy_from(bytes))
        .map_err(vortex_error)?;
    let array = runtime
        .block_on(async { file.scan()?.into_array_stream()?.read_all().await })
        .map_err(vortex_error)?;
    let mut ctx = session.create_execution_ctx();
    let mut rows = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        let scalar = array
            .execute_scalar(index, &mut ctx)
            .map_err(vortex_error)?;
        // `encode` only ever writes a `VarBinArray` of non-null strings, so a
        // null or non-utf8 scalar here is a corrupted or mistyped shard: fail
        // it closed instead of decoding a different-but-plausible row.
        let value = scalar
            .value()
            .ok_or_else(|| vortex_error(format!("shard row {index} is null")))?;
        match value {
            ScalarValue::Utf8(text) => rows.push(text.as_str().to_owned()),
            _ => {
                return Err(vortex_error(format!(
                    "shard row {index} is not a utf8 scalar"
                )))
            }
        }
    }
    Ok(rows)
}

fn vortex_error(error: impl std::fmt::Display) -> Error {
    Error::storage(format!("vortex codec: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn a_zero_row_shard_still_carries_the_file_framing() {
        // The fail-closed premise: every shard `encode` writes is a real
        // vortex file, zero rows included — so zero bytes on disk can only
        // be a torn write.
        let bytes = encode(&[]).expect("encode of zero rows");
        assert!(!bytes.is_empty());
        assert_eq!(decode(&bytes).expect("decode"), Vec::<String>::new());
    }

    #[test]
    fn zero_bytes_fail_closed_instead_of_answering_zero_rows() {
        assert!(decode(&[]).is_err());
    }
}
