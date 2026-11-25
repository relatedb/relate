//! Body framing for the Query API reply seam (ADR-0386).
//!
//! A reply's framing is decided before its head goes out, from the
//! uncompressed serialized size: [`count_json`] runs one bounded counting
//! pass that keeps at most [`STREAM_THRESHOLD_BYTES`] - 1 bytes, so a body
//! that fits is written one-shot under `Content-Length` from exactly those
//! bytes, and a body at or above the threshold re-serializes straight into
//! the RFC 9112 chunked writer ([`write_chunked`]) — never into a
//! body-sized `Vec` copy.
//!
//! The body's wire shape — the success envelope's parts and their member
//! order — is [`crate::v2_envelope`]'s; this module only measures and
//! frames it. Every chunked body is framed through a fixed-size frame
//! buffer, and a producer write larger than that buffer is sliced into
//! frames of the same size, so no frame payload ever exceeds
//! [`FRAME_BUFFER_BYTES`]. A chunk boundary is therefore a buffer flush or a
//! slice edge — a transport detail clients must not depend on — never a row
//! or member boundary.

use std::io::{self, BufWriter, Write};

use flate2::write::GzEncoder;
use serde::Serialize;

use crate::content_encoding::{gzip_encoder, BodyEncoding};
use crate::http::{write_head, Framing};
use crate::v2_envelope::ReplyBody;

/// The uncompressed serialized body size at which a reply switches from the
/// one-shot `Content-Length` body to `Transfer-Encoding: chunked`, inclusive
/// (ADR-0386 §1). Below it the buffered path's extra copy stays at most
/// 64 KiB per connection; at or above it the body streams through the frame
/// writer. The one declaration: the framing decision (`count_json`) and its
/// boundary tests both read this constant.
pub(crate) const STREAM_THRESHOLD_BYTES: usize = 65_536;

/// The capacity of the buffer every chunked body passes through (ADR-0386
/// §2), and the hard bound on one frame's payload: the serialized bytes
/// accumulate in a [`BufWriter`] of this size and each flush becomes one
/// chunk frame, while [`ChunkWriter::write`] slices any producer write larger
/// than the buffer (a `serde_json` string scalar, flate2's 32 KiB zio buffer)
/// into frames of at most this size. A chunk boundary is thus a transport
/// detail clients must not depend on, and the frame count is bounded by the
/// body's size rather than by its row count.
pub(crate) const FRAME_BUFFER_BYTES: usize = 8 * 1024;

/// The counting pass's framing decision (ADR-0386 §2/§5).
pub(crate) enum CountedJson {
    /// Below [`STREAM_THRESHOLD_BYTES`]: these are the exact serialized
    /// bytes, written one-shot under `Content-Length`.
    OneShot(Vec<u8>),
    /// At or above [`STREAM_THRESHOLD_BYTES`]: the caller re-serializes the
    /// body into its streaming representation.
    Stream,
}

/// Decide the framing from the uncompressed serialized size, before any head
/// byte is written (ADR-0386 §2/§5): serialize `body` into a writer bounded
/// at the largest one-shot size, return those bytes when it fits — they are
/// the one-shot body, byte for byte — and report [`CountedJson::Stream`] the
/// moment the body would reach the threshold, so the counting pass never
/// completes a streaming body's copy. [`write_chunked`] serializes through the
/// same [`Serialize`](serde::Serialize) implementation, so the measured
/// length is the written length.
/// The one threshold decision (ADR-0386). The standalone server turns it into HTTP
/// framing. [`crate::framing::frame_json`] turns it into unframed embedder
/// payloads. Both stop the counting pass at the same byte.
pub(crate) fn count_json<T: Serialize>(body: &T) -> io::Result<CountedJson> {
    let mut counted = CountingWriter::new(STREAM_THRESHOLD_BYTES - 1);
    match serde_json::to_writer(&mut counted, body) {
        Ok(()) => Ok(CountedJson::OneShot(counted.into_bytes())),
        Err(_) if counted.exceeded() => Ok(CountedJson::Stream),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Write `body` as RFC 9112 §7.1 chunk frames (ADR-0386 §2/§4). The
/// serialized bytes pass through a fixed-size [`FRAME_BUFFER_BYTES`] buffer
/// and any producer write larger than that buffer is sliced by
/// [`ChunkWriter::write`], so every frame payload is at most
/// [`FRAME_BUFFER_BYTES`] — a chunk boundary is a buffer flush or a slice
/// edge, a transport detail clients must not depend on. With
/// [`BodyEncoding::Gzip`] the compressor composes above that buffer: the
/// envelope feeds the gzip encoder and the compressed bytes are the frame
/// payloads. The body ends with one zero-size last chunk and no trailer
/// section — [`ChunkedBody::finish`] flushes the frame buffer, the last chunk and
/// the sink, so there is no second flush here.
pub(crate) fn write_chunked(
    sink: &mut impl Write,
    body: &ReplyBody,
    encoding: BodyEncoding,
) -> io::Result<()> {
    let mut frames = ChunkedBody::new(&mut *sink, encoding);
    serde_json::to_writer(&mut frames, body).map_err(io::Error::other)?;
    frames.finish()
}

/// Write a chunked reply's head (ADR-0386 §4): the streamed body's encoding
/// is negotiated from `accept_encoding` by weight alone
/// ([`BodyEncoding::for_streamed_body`] — the final size is unknown before
/// the head), its `content-encoding` header joins `extra_headers`, and the
/// head goes out with `Transfer-Encoding: chunked`. Returns the encoding the
/// body must be written in. The one chunked head: the whole-body reply
/// ([`write_chunked`]) and the statement streamer (`crate::streamed`) both
/// open their bodies through it.
pub(crate) fn write_chunked_head(
    writer: &mut impl Write,
    status: u16,
    content_type: &str,
    mut extra_headers: Vec<(String, String)>,
    accept_encoding: Option<&str>,
) -> io::Result<BodyEncoding> {
    let encoding = BodyEncoding::for_streamed_body(accept_encoding);
    encoding.push_header(&mut extra_headers);
    write_head(
        writer,
        status,
        content_type,
        Framing::Chunked,
        &extra_headers,
    )?;
    Ok(encoding)
}

/// The one frame composition (ADR-0386 §2): the fixed-size frame buffer
/// over a frame writer `F`, with the gzip encoder above it when negotiated.
/// The standalone server's `F` is the RFC 9112 [`ChunkWriter`]
/// ([`ChunkedBody`]); an embedder's is `crate::framing`'s unframed payload
/// writer. [`Self::finish`] flushes the compressor and the buffer and hands
/// the frame writer back for its own ending.
pub(crate) enum EncodedFrames<F: Write> {
    Identity(BufWriter<F>),
    Gzip(GzEncoder<BufWriter<F>>),
}

impl<F: Write> EncodedFrames<F> {
    pub(crate) fn new(frames: F, encoding: BodyEncoding) -> Self {
        let buffered = BufWriter::with_capacity(FRAME_BUFFER_BYTES, frames);
        match encoding {
            BodyEncoding::Gzip => Self::Gzip(gzip_encoder(buffered)),
            BodyEncoding::Identity => Self::Identity(buffered),
        }
    }

    /// Flush the compressor (when present) and the frame buffer into the
    /// frame writer, and return it.
    pub(crate) fn finish(self) -> io::Result<F> {
        let buffered = match self {
            Self::Gzip(encoder) => encoder.finish()?,
            Self::Identity(buffered) => buffered,
        };
        buffered.into_inner().map_err(|error| error.into_error())
    }
}

impl<F: Write> Write for EncodedFrames<F> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Gzip(encoder) => encoder.write(buf),
            Self::Identity(buffered) => buffered.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Gzip(encoder) => encoder.flush(),
            Self::Identity(buffered) => buffered.flush(),
        }
    }
}

/// An open chunked body: the [`EncodedFrames`] composition over the RFC
/// 9112 chunk writer. [`write_chunked`] feeds it one whole body; the
/// statement streamer (`crate::streamed`) feeds it the envelope piece by
/// piece as the engine's batches arrive. Either way the body ends through
/// [`Self::finish`].
pub(crate) struct ChunkedBody<W: Write>(EncodedFrames<ChunkWriter<W>>);

impl<W: Write> ChunkedBody<W> {
    pub(crate) fn new(sink: W, encoding: BodyEncoding) -> Self {
        Self(EncodedFrames::new(ChunkWriter::new(sink), encoding))
    }

    /// End the body: flush the compressor (when present) and the frame
    /// buffer — the buffered bytes become the last frames — then write the
    /// zero-size last chunk and flush the sink.
    pub(crate) fn finish(self) -> io::Result<()> {
        self.0.finish()?.finish()
    }
}

impl<W: Write> Write for ChunkedBody<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// The one frame slicer (ADR-0386 §2): hand `buf` to `frame` in payloads of
/// at most [`FRAME_BUFFER_BYTES`]. An empty `buf` yields no frame at all.
/// Both frame writers — the RFC 9112 [`ChunkWriter`] and `crate::framing`'s
/// unframed payload writer — slice through it.
pub(crate) fn for_each_frame(
    buf: &[u8],
    mut frame: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<usize> {
    for payload in buf.chunks(FRAME_BUFFER_BYTES) {
        frame(payload)?;
    }
    Ok(buf.len())
}

/// A [`Write`] that only counts the bytes written to it: the encoded size of
/// a value, measured without keeping it (the statement streamer's
/// buffered-rows measure, `crate::streamed`). [`CountingWriter`] is the
/// keeping, bounded form the one-shot framing decision needs.
#[derive(Default)]
pub(crate) struct ByteCount(usize);

impl ByteCount {
    pub(crate) fn bytes(&self) -> usize {
        self.0
    }
}

impl Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The counting pass's writer (ADR-0386 §5): it keeps the body's bytes while
/// they stay under `limit` and records [`Self::exceeded`] on the byte that
/// would cross it, so the serializer stops at the threshold instead of
/// finishing a body-sized copy just to measure it.
pub(crate) struct CountingWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl CountingWriter {
    /// A counting pass bounded at `limit` bytes — the known largest one-shot
    /// size, reserved up front so the serializer's writes never re-grow the
    /// buffer.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
            exceeded: false,
        }
    }

    /// Whether a write was refused at the limit — the one sign that the body
    /// streams, told apart from a serialization error.
    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + buf.len() > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("the body reaches the stream threshold"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The RFC 9112 §7.1 frame writer (ADR-0386 §2/§4): every non-empty write is
/// one or more chunks — a lowercase-hex size line, the exact bytes, CRLF per
/// slice — and [`Self::finish`] writes the zero-size last chunk with no
/// trailer section. A write larger than [`FRAME_BUFFER_BYTES`] is sliced into
/// frames of that size, so the bound holds for every producer: `BufWriter`
/// forwards any write at least its capacity straight through, which is how
/// `serde_json`'s large string scalars and flate2's 32 KiB zio buffer would
/// otherwise reach the wire as oversized frames.
struct ChunkWriter<W: Write> {
    inner: W,
}

impl<W: Write> ChunkWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Terminate the chunked body with the zero-size last chunk and flush
    /// the sink. The finished writer has nothing to hand back: the caller
    /// owns it and the body ends here, so the result is the write outcome.
    fn finish(mut self) -> io::Result<()> {
        self.inner.write_all(b"0\r\n\r\n")?;
        self.inner.flush()
    }
}

impl<W: Write> Write for ChunkWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // An empty buffer writes nothing at all: a zero-size chunk is the
        // last chunk and would terminate the body early.
        let inner = &mut self.inner;
        for_each_frame(buf, |frame| {
            write!(inner, "{:x}\r\n", frame.len())?;
            inner.write_all(frame)?;
            inner.write_all(b"\r\n")
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use relate_test_support::loopback::{dechunk, gunzip, max_frame_size};
    use serde_json::{json, Map, Value as Json};

    use super::*;

    /// A whole body of exactly `len` serialized bytes: the object's
    /// `{"x":"` opener and `"}` closer take eight.
    fn string_body(len: usize) -> ReplyBody {
        ReplyBody::new([("x", Json::String("a".repeat(len - 8)))])
    }

    /// The success envelope in the shape its one constructor builds: a body
    /// list whose `data` member (fields, then the rows) comes first, followed
    /// by `tail`.
    fn success_envelope(
        fields: Json,
        rows: Vec<Json>,
        tail: impl IntoIterator<Item = (&'static str, Json)>,
    ) -> ReplyBody {
        let mut data = Map::new();
        data.insert("fields".to_string(), fields);
        data.insert("values".to_string(), Json::Array(rows));
        let mut members = vec![("data", Json::Object(data))];
        members.extend(tail);
        ReplyBody::new(members)
    }

    #[test]
    fn framing_starts_at_the_threshold() {
        // One byte below the threshold stays one-shot, with those exact
        // bytes as the body.
        let below = string_body(STREAM_THRESHOLD_BYTES - 1);
        match count_json(&below).unwrap() {
            CountedJson::OneShot(bytes) => {
                assert_eq!(bytes.len(), STREAM_THRESHOLD_BYTES - 1);
                assert_eq!(bytes, serde_json::to_vec(&below).unwrap());
            }
            CountedJson::Stream => panic!("one byte below the threshold stays one-shot"),
        }
        // Exactly the threshold streams (inclusive, ADR-0386 §1).
        assert!(matches!(
            count_json(&string_body(STREAM_THRESHOLD_BYTES)).unwrap(),
            CountedJson::Stream
        ));
        // And so does everything above it.
        assert!(matches!(
            count_json(&string_body(STREAM_THRESHOLD_BYTES * 4)).unwrap(),
            CountedJson::Stream
        ));
    }

    #[test]
    fn the_counting_writer_stops_at_its_limit() {
        // The counting pass holds at most `limit` bytes: the write that would
        // cross it is refused and recorded, so the serializer stops there
        // instead of materializing the body.
        let mut counted = CountingWriter::new(4);
        counted.write_all(b"1234").unwrap();
        assert!(!counted.exceeded());
        assert!(counted.write_all(b"5").is_err());
        assert!(counted.exceeded());
        assert_eq!(counted.into_bytes(), b"1234");
    }

    #[test]
    fn a_large_success_envelope_streams_rows_in_bounded_frames() {
        // Above the threshold the counting pass gives up at the limit, and
        // the chunked write streams the envelope data-first (fields, then the
        // rows) through the fixed-size frame buffer: frames are bounded by
        // the body's size, never one per row (ADR-0386 §3/§5).
        let body = success_envelope(
            json!(["n"]),
            (1..=20_000).map(|n| json!([n])).collect(),
            [("bookmarks", Json::Array(Vec::new()))],
        );
        assert!(matches!(count_json(&body).unwrap(), CountedJson::Stream));

        let mut wire = Vec::new();
        write_chunked(&mut wire, &body, BodyEncoding::Identity).unwrap();
        // Every frame — not just the first — is bounded by the fixed-size
        // frame buffer, so frames track the body's size.
        let largest = max_frame_size(&wire);
        assert!(
            largest <= FRAME_BUFFER_BYTES,
            "every frame is bounded by the frame buffer, got {largest}"
        );
        let (reassembled, frames) = dechunk(&wire);
        assert!(
            frames > 1 && frames < 1_000,
            "a bounded number of frames, got {frames}"
        );
        let text = String::from_utf8(reassembled.clone()).expect("the body is UTF-8");
        assert!(
            text.starts_with(r#"{"data":{"fields":["n"],"values":[[1],[2],[3],"#),
            "the data member streams first: {}",
            &text[..80]
        );
        assert!(
            text.ends_with(r#"},"bookmarks":[]}"#),
            "the tail follows the rows: {}",
            &text[text.len() - 40..]
        );
        let parsed: Json = serde_json::from_slice(&reassembled).unwrap();
        assert_eq!(parsed, serde_json::to_value(&body).unwrap());
    }

    #[test]
    fn a_large_whole_body_chunk_decodes_to_its_json() {
        // A body that is not the success envelope (an error envelope over a
        // huge message, a large transaction answer) is the same member list
        // and still arrives in bounded frames that reassemble exactly.
        let body = ReplyBody::new([(
            "errors",
            Json::Array(
                (1..=20_000)
                    .map(|n| json!({"code": "42000", "message": n}))
                    .collect(),
            ),
        )]);
        assert!(matches!(count_json(&body).unwrap(), CountedJson::Stream));

        let mut wire = Vec::new();
        write_chunked(&mut wire, &body, BodyEncoding::Identity).unwrap();
        let largest = max_frame_size(&wire);
        assert!(
            largest <= FRAME_BUFFER_BYTES,
            "every frame is bounded by the frame buffer, got {largest}"
        );
        let (reassembled, frames) = dechunk(&wire);
        assert!(
            frames > 1 && frames < 1_000,
            "a bounded number of frames, got {frames}"
        );
        assert_eq!(
            serde_json::from_slice::<Json>(&reassembled).unwrap(),
            serde_json::to_value(&body).unwrap(),
            "the frames reassemble to the whole body's JSON"
        );
    }

    #[test]
    fn one_large_string_scalar_still_streams_in_bounded_frames() {
        // `serde_json` writes a string scalar as one `write_all`, and a
        // `BufWriter` forwards any write at least its capacity straight
        // through, so a 300 KB value reaches the frame writer as one 300 KB
        // buffer. The frame writer slices it: the documented 8 KiB bound
        // holds for the producers that bypass the buffer, not only for the
        // serializer's many small token writes.
        let body = string_body(300 * 1024);
        assert!(matches!(count_json(&body).unwrap(), CountedJson::Stream));

        let mut wire = Vec::new();
        write_chunked(&mut wire, &body, BodyEncoding::Identity).unwrap();
        let largest = max_frame_size(&wire);
        assert!(
            largest <= FRAME_BUFFER_BYTES,
            "a 300 KB scalar must not become one 300 KB frame, got {largest}"
        );
        let (reassembled, frames) = dechunk(&wire);
        assert!(frames > 1, "the scalar spans several frames, got {frames}");
        assert_eq!(
            serde_json::from_slice::<Json>(&reassembled).unwrap(),
            serde_json::to_value(&body).unwrap(),
            "the sliced frames reassemble to the scalar's JSON"
        );
    }

    #[test]
    fn the_frame_writer_slices_a_write_larger_than_its_buffer() {
        // The frame writer's own contract, independent of any producer: a
        // single write three buffers wide becomes four frames of at most
        // FRAME_BUFFER_BYTES, and the last chunk still terminates the body.
        let payload: Vec<u8> = (0..FRAME_BUFFER_BYTES * 3 + 7)
            .map(|offset| (offset % 251) as u8)
            .collect();
        let mut wire = Vec::new();
        let mut frames = ChunkWriter::new(&mut wire);
        frames.write_all(&payload).unwrap();
        frames.finish().unwrap();

        let largest = max_frame_size(&wire);
        assert_eq!(
            largest, FRAME_BUFFER_BYTES,
            "the full slices are FRAME_BUFFER_BYTES and the remainder is smaller"
        );
        let (reassembled, count) = dechunk(&wire);
        assert_eq!(count, 4, "three full slices and the remainder");
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn gzip_composes_above_the_frame_writer() {
        // The compressor writes into the frame buffer, and its compressed
        // bytes become the chunk payloads: the frames inflate to the streamed
        // envelope (data first, then the tail in insertion order), and the
        // row order survives both compositions.
        let mut body = success_envelope(
            json!(["n"]),
            vec![json!([1]), json!([2]), json!([3])],
            [
                ("counters", json!({"nodes_created": 1})),
                ("bookmarks", json!([])),
            ],
        );
        body.insert_member("transaction", json!({"id": "t1"}));
        let mut wire = Vec::new();
        write_chunked(&mut wire, &body, BodyEncoding::Gzip).unwrap();
        let largest = max_frame_size(&wire);
        assert!(
            largest <= FRAME_BUFFER_BYTES,
            "gzip frames are bounded by the frame buffer, got {largest}"
        );
        let (compressed, frames) = dechunk(&wire);
        assert!(frames >= 1, "the compressor's writes are frames");
        assert_eq!(
            String::from_utf8(gunzip(&compressed)).unwrap(),
            concat!(
                r#"{"data":{"fields":["n"],"values":[[1],[2],[3]]},"#,
                r#""counters":{"nodes_created":1},"bookmarks":[],"#,
                r#""transaction":{"id":"t1"}}"#
            )
        );
    }

    #[test]
    fn a_large_gzip_envelope_stays_within_the_frame_bound() {
        // flate2's write encoder dumps its 32 KiB zio buffer in one write, and
        // a `BufWriter` forwards any write at least its capacity straight
        // through, so a large compressed body reaches the frame writer as
        // 32 KiB buffers. The frame writer slices them: every frame of the
        // compressed representation is bounded, and it still inflates to the
        // envelope in wire order.
        let body = success_envelope(
            json!(["n"]),
            (1..=20_000).map(|n| json!([n])).collect(),
            [("bookmarks", Json::Array(Vec::new()))],
        );
        assert!(matches!(count_json(&body).unwrap(), CountedJson::Stream));

        let mut wire = Vec::new();
        write_chunked(&mut wire, &body, BodyEncoding::Gzip).unwrap();
        let largest = max_frame_size(&wire);
        assert!(
            largest <= FRAME_BUFFER_BYTES,
            "the compressor's 32 KiB writes must be sliced, got {largest}"
        );
        let (compressed, frames) = dechunk(&wire);
        assert!(
            frames > 1,
            "the compressed envelope spans several frames, got {frames}"
        );
        let inflated = gunzip(&compressed);
        assert_eq!(
            serde_json::from_slice::<Json>(&inflated).unwrap(),
            serde_json::to_value(&body).unwrap(),
            "the compressed frames inflate to the envelope"
        );
        let text = String::from_utf8(inflated).expect("the envelope is UTF-8");
        assert!(
            text.starts_with(r#"{"data":{"fields":["n"],"values":[[1],[2],[3],"#),
            "the data member streams first"
        );
        assert!(
            text.ends_with(r#"},"bookmarks":[]}"#),
            "the tail follows the rows"
        );
    }
}
