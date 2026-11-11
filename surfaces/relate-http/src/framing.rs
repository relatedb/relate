//! Reusable Query API JSON response framing.
//!
//! The standalone server writes HTTP/1.1 chunk frames itself. Clients
//! implementing embedded Relate may use Hyper, which owns those frames, so
//! this module emits bounded representation bytes instead. Both paths share
//! the threshold, gzip negotiation, and serializer.

use std::io::{self, BufWriter, Write};

use serde::Serialize;

pub use crate::content_encoding::BodyEncoding;
use crate::content_encoding::{encode_body, gzip_encoder};
use crate::stream::{count_json, CountedJson, FRAME_BUFFER_BYTES, STREAM_THRESHOLD_BYTES};

/// The uncompressed serialized response size at which a response streams.
pub const STREAM_THRESHOLD: usize = STREAM_THRESHOLD_BYTES;

/// The maximum payload size emitted by [`write_stream`].
pub const FRAME_BYTES: usize = FRAME_BUFFER_BYTES;

/// A response's framing decision, made before its HTTP headers are written.
pub enum FramedJson {
    /// A complete below-threshold representation, optionally gzip encoded.
    OneShot {
        bytes: Vec<u8>,
        content_encoding: BodyEncoding,
    },
    /// The body must stream. The caller writes it through [`write_stream`].
    Streamed { content_encoding: BodyEncoding },
}

/// Decide a JSON response's framing and one-shot representation.
///
/// Below the threshold, gzip is selected only when the client accepts it and
/// compression shrinks the body. At or above the threshold, gzip is selected
/// from the `Accept-Encoding` weight alone because the final size is unknown
/// before the response head is sent.
pub fn frame_json<T: Serialize>(body: &T, accept_encoding: Option<&str>) -> io::Result<FramedJson> {
    match count_json(body)? {
        CountedJson::OneShot(bytes) => {
            let encoded = encode_body(accept_encoding, bytes)?;
            Ok(FramedJson::OneShot {
                bytes: encoded.bytes,
                content_encoding: encoded.encoding,
            })
        }
        CountedJson::Stream => {
            let encoding = BodyEncoding::for_streamed_body(accept_encoding);
            Ok(FramedJson::Streamed {
                content_encoding: encoding,
            })
        }
    }
}

/// Serialize JSON into bounded representation payloads.
///
/// `sink` receives unframed bytes. A Hyper/Axum body turns those writes into
/// HTTP/1.1 chunks; the standalone Relate server instead uses
/// `stream::write_chunked`, which adds RFC 9112 framing itself.
pub fn write_stream<T: Serialize>(
    sink: &mut impl Write,
    body: &T,
    content_encoding: BodyEncoding,
) -> io::Result<()> {
    let mut buffered = BufWriter::with_capacity(FRAME_BUFFER_BYTES, FrameWriter::new(sink));
    if content_encoding == BodyEncoding::Gzip {
        let mut encoder = gzip_encoder(buffered);
        serde_json::to_writer(&mut encoder, body).map_err(io::Error::other)?;
        buffered = encoder.finish()?;
    } else {
        serde_json::to_writer(&mut buffered, body).map_err(io::Error::other)?;
    }
    buffered.flush()
}

/// Slices every write to the public frame bound. Unlike `stream::ChunkWriter`,
/// this does not add HTTP/1.1 chunk envelopes: the embedder's HTTP stack does.
/// Both slice at [`FRAME_BUFFER_BYTES`].
struct FrameWriter<'a, W: Write> {
    inner: &'a mut W,
}

impl<'a, W: Write> FrameWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for FrameWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for frame in buf.chunks(FRAME_BUFFER_BYTES) {
            self.inner.write_all(frame)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_json_writes_bounded_unframed_payloads() {
        let body = serde_json::json!({ "value": "a".repeat(STREAM_THRESHOLD) });
        let FramedJson::Streamed { content_encoding } = frame_json(&body, None).unwrap() else {
            panic!("threshold-sized body streams");
        };
        assert_eq!(content_encoding, BodyEncoding::Identity);
        let mut writes = Writes(Vec::new());
        write_stream(&mut writes, &body, content_encoding).unwrap();
        assert!(writes.len() > 1);
        assert!(writes.iter().all(|payload| payload.len() <= FRAME_BYTES));
        let bytes = writes.concat();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            body
        );
    }

    struct Writes(Vec<Vec<u8>>);

    impl Write for Writes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.push(bytes.to_vec());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl std::ops::Deref for Writes {
        type Target = Vec<Vec<u8>>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl std::ops::DerefMut for Writes {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
}
