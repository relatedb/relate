//! Response-body compression for the Query API reply seam (ADR-0385).
//!
//! One `Accept-Encoding` parse (RFC 9110 §12.5.3) and one negotiation entry
//! point decide between the two representations this surface can produce:
//! identity and gzip. The policy can never fail a request — identity is
//! always available, so a missing or empty header, an unsupported coding or
//! `q=0` all select identity instead of a 406 (the separate `Accept`
//! media-type negotiation and its 406 are untouched).
//!
//! Gzip is selected only when the client's weight for it is positive, the
//! serialized body reaches [`GZIP_THRESHOLD_BYTES`], and the compressed
//! output is smaller than the input (ADR-0385 §1/§5) — [`encode_body`] is
//! the one-shot entry point and owns those three checks. Above ADR-0386's
//! streaming threshold the output-shrink check cannot run before the head
//! (the compressed size is unknown), so there gzip is selected by weight
//! alone and wraps the chunked writer (`crate::stream`) through
//! [`BodyEncoding::for_streamed_body`] — the one entry point whose caller
//! has already established the size precondition. [`accepts_gzip`] is the
//! one weight decision both paths consult.

use std::io::{self, Write};

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::http::RequestHead;

/// The smallest serialized body that is worth compressing (ADR-0385 §5,
/// inclusive): below it gzip's framing and encoder cost outweigh the saving.
pub(crate) const GZIP_THRESHOLD_BYTES: usize = 1024;

/// The `Vary` value every negotiated reply advertises: its body depends on
/// the request's `Accept-Encoding` (ADR-0385 §2).
pub(crate) const VARY_ACCEPT_ENCODING: &str = "Accept-Encoding";

/// The response-body representations this surface can produce. Exactly two
/// exist; every other coding in `Accept-Encoding` is parsed and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyEncoding {
    /// The serialized JSON bytes, uncompressed.
    Identity,
    /// The serialized JSON bytes under `content-encoding: gzip`.
    Gzip,
}

impl BodyEncoding {
    /// The `content-encoding` header value for this representation —
    /// `None` for identity, where the header is omitted (never
    /// `content-encoding: identity`).
    pub fn header_value(self) -> Option<&'static str> {
        match self {
            Self::Identity => None,
            Self::Gzip => Some("gzip"),
        }
    }

    /// The representation decision for a **streamed** reply, whose compressed
    /// size cannot be known before its head (ADR-0386 §4): gzip when the
    /// client's weight for it is positive, identity otherwise.
    ///
    /// Precondition: the caller has already established that the body clears
    /// the streaming threshold, which is at or above
    /// [`GZIP_THRESHOLD_BYTES`] — so the size and shrink checks
    /// [`encode_body`] performs for the one-shot path cannot select gzip for
    /// a body this decision would reject. A one-shot body must go through
    /// [`encode_body`], never through this function.
    pub(crate) fn for_streamed_body(accept_encoding: Option<&str>) -> Self {
        if accepts_gzip(accept_encoding) {
            Self::Gzip
        } else {
            Self::Identity
        }
    }
}

/// One encoded response body: the bytes to send and the representation
/// they carry.
pub(crate) struct EncodedBody {
    /// The representation of [`Self::bytes`].
    pub(crate) encoding: BodyEncoding,
    /// The bytes to put on the wire.
    pub(crate) bytes: Vec<u8>,
}

/// One reply's response-encoding policy (ADR-0385 §2/§6). It makes three
/// decisions for the reply it is attached to:
///
/// 1. whether the reply is inside the representation-negotiation seam at all
///    — [`Self::Excluded`] for the public `GET /` discovery document,
///    [`Self::Negotiated`] for every Query API reply the seam writes, the
///    error replies included. The body-less `204` CORS preflight is a Query
///    API reply but not a seam reply: `server::write_preflight` answers it
///    before the seam, so no policy is attached and it never carries `Vary`
///    or an encoding header;
/// 2. which request value the one-shot and streaming decisions read — the raw
///    `Accept-Encoding` header, carried verbatim (`None` = the request sent
///    none), so the `q` grammar is parsed once, in [`accepts_gzip`];
/// 3. whether the reply advertises `Vary: Accept-Encoding` — a negotiated
///    reply always does, identity included, because a different
///    `Accept-Encoding` would change its body; the excluded discovery
///    document never does.
pub(crate) enum ReplyEncodingPolicy {
    /// Negotiate from the request's raw `Accept-Encoding` header (`None` =
    /// the request carried none) and advertise `Vary: Accept-Encoding`.
    Negotiated(Option<String>),
    /// Identity, no `Vary`: the discovery document.
    Excluded,
}

impl ReplyEncodingPolicy {
    /// The policy for one reply that will actually be written: `public` is
    /// the gate's public-route classification (the Query API's `GET /`
    /// discovery document, plus any route the extension declares public —
    /// both unauthenticated and outside the seam), carried as data by the
    /// gate outcome — no consumer re-derives it. Every other Query API reply
    /// negotiates. Only the gate outcomes that write a seam reply call this;
    /// the body-less `204` preflight is answered directly by
    /// `server::write_preflight` and never reaches this policy.
    pub(crate) fn for_head(head: &RequestHead, public: bool) -> Self {
        if public {
            Self::Excluded
        } else {
            Self::Negotiated(head.header_list("accept-encoding"))
        }
    }

    /// The policy for a reply whose request head never parsed (the gate's
    /// `Malformed` outcome): nothing is known about `Accept-Encoding`, so the
    /// reply negotiates to identity — but it is still a Query API reply, on
    /// the seam and advertising `Vary`, unlike the excluded discovery
    /// document.
    pub(crate) fn for_unparsed_head() -> Self {
        Self::Negotiated(None)
    }
}

/// The one negotiation entry point (ADR-0385 §1): decide identity-vs-gzip
/// from the raw `Accept-Encoding` header and the serialized body, and
/// return the bytes to send. Gzip is chosen only when the client accepts
/// it, the body reaches [`GZIP_THRESHOLD_BYTES`], and the compressed output
/// is smaller than the input; every other case is the identity bytes
/// unchanged.
pub(crate) fn encode_body(
    accept_encoding: Option<&str>,
    payload: Vec<u8>,
) -> io::Result<EncodedBody> {
    if !accepts_gzip(accept_encoding) || payload.len() < GZIP_THRESHOLD_BYTES {
        return Ok(EncodedBody {
            encoding: BodyEncoding::Identity,
            bytes: payload,
        });
    }
    let compressed = gzip(&payload)?;
    if compressed.len() < payload.len() {
        Ok(EncodedBody {
            encoding: BodyEncoding::Gzip,
            bytes: compressed,
        })
    } else {
        Ok(EncodedBody {
            encoding: BodyEncoding::Identity,
            bytes: payload,
        })
    }
}

/// Encode `payload` as one gzip member at the shared encoder level: the
/// encode runs on the connection thread, so latency beats the last few
/// percent of ratio (ADR-0385 §1). The output buffer is reserved from the
/// known payload size up front, so the encoder's writes do not re-grow it.
fn gzip(payload: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = gzip_encoder(Vec::with_capacity(payload.len()));
    encoder.write_all(payload)?;
    encoder.finish()
}

/// The one gzip encoder every gzip body uses — one-shot below ADR-0386's
/// streaming threshold and streaming above it: `flate2::Compression::fast`
/// (level 1), because the encode runs on the connection thread and latency
/// beats the last few percent of ratio (ADR-0385 §1).
pub(crate) fn gzip_encoder<W: Write>(sink: W) -> GzEncoder<W> {
    GzEncoder::new(sink, Compression::fast())
}

/// Whether the client's weight for gzip is positive: the explicit
/// `gzip`/`x-gzip` element when present, else the `*` element, else unset
/// (identity). A missing, empty or whitespace-only header selects identity;
/// `gzip;q=0` overrides a positive `*`, and a positive explicit `gzip`
/// overrides `*;q=0` (ADR-0385 §1/§3/§4). The one weight decision: the
/// one-shot path pairs it with the size and shrink checks in [`encode_body`],
/// the streaming path (ADR-0386) with the body size alone.
pub(crate) fn accepts_gzip(accept_encoding: Option<&str>) -> bool {
    let Some(header) = accept_encoding else {
        return false;
    };
    // The first explicit gzip entry and the first wildcard entry; the
    // explicit one wins in both directions.
    let mut gzip: Option<bool> = None;
    let mut wildcard: Option<bool> = None;
    for element in header.split(',') {
        let element = element.trim();
        if element.is_empty() {
            // An empty list element is ignored, never read as the wildcard.
            continue;
        }
        let mut parts = element.split(';');
        let coding = Coding::parse(crate::http::RequestHead::media_token(
            parts.next().expect("str::split always yields one item"),
        ));
        let positive = positive_weight(parts);
        match coding {
            Coding::Gzip => {
                gzip.get_or_insert(positive);
            }
            Coding::Wildcard => {
                wildcard.get_or_insert(positive);
            }
            Coding::Other => {}
        }
    }
    // Neither an explicit gzip entry nor a wildcard is identity: the
    // weight's default is "not selected".
    gzip.or(wildcard).unwrap_or_default()
}

/// The codings an `Accept-Encoding` element can name that change the
/// decision: the two spellings of gzip this surface produces, the wildcard,
/// and every other coding (parsed, never selected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coding {
    Gzip,
    Wildcard,
    Other,
}

impl Coding {
    /// Classify one coding token. `gzip`'s legacy alias `x-gzip` names the
    /// same representation (ADR-0385 §1).
    fn parse(token: &str) -> Self {
        if token.eq_ignore_ascii_case("gzip") || token.eq_ignore_ascii_case("x-gzip") {
            Self::Gzip
        } else if token == "*" {
            Self::Wildcard
        } else {
            Self::Other
        }
    }
}

/// Whether the element's weight is positive: its first `q` parameter (name
/// case-insensitive) decides, and an element with no `q` weighs the default
/// 1.0. An unparsable `q`, or a `q` without a value, is `q=0` (ADR-0385 §1),
/// so a malformed weight can only ever exclude a coding.
fn positive_weight<'a>(parameters: impl Iterator<Item = &'a str>) -> bool {
    for parameter in parameters {
        let parameter = parameter.trim();
        match parameter.split_once('=') {
            Some((name, value)) if name.trim().eq_ignore_ascii_case("q") => {
                return weight_is_positive(value.trim());
            }
            // A bare `q` declares the parameter without a value: unparsable,
            // so q=0.
            None if parameter.eq_ignore_ascii_case("q") => return false,
            _ => {}
        }
    }
    true
}

/// Whether an RFC 9110 §12.4.2 weight selects its coding: `1`, `1.000`, or
/// `0.<fraction>` with some nonzero digit. The `1*3DIGIT` fraction grammar is
/// checked once, before the whole part decides, so a nonzero fraction after a
/// `1` (`1.5`, `1.001`) is out of range and `0.000` is zero — neither
/// selects.
fn weight_is_positive(raw: &str) -> bool {
    let (whole, fraction) = match raw.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (raw, None),
    };
    let fraction = match fraction {
        None => None,
        Some(fraction)
            if !fraction.is_empty()
                && fraction.len() <= 3
                && fraction.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(fraction)
        }
        // Out of the qvalue grammar (`.5`, `0.`, `0.1234`, `1.0000`): q=0.
        Some(_) => return false,
    };
    match (whole, fraction) {
        ("1", None) => true,
        // `1.000`, in any shorter all-zero spelling, is exactly 1.0.
        ("1", Some(fraction)) => fraction.bytes().all(|b| b == b'0'),
        // `0.<fraction>` is positive only with a nonzero digit.
        ("0", Some(fraction)) => fraction.bytes().any(|b| b != b'0'),
        // `0` itself, and every whole part outside the grammar (`2`, `-1`,
        // ``).
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use relate_test_support::loopback::gunzip;

    use super::*;

    /// The `Accept-Encoding` parse table: the one place the negotiation
    /// rules (RFC 9110 list syntax, the gzip alias, `q=0`, the wildcard
    /// override, unsupported codings) are pinned.
    #[test]
    fn accept_encoding_parse_table() {
        let cases: &[(Option<&str>, bool)] = &[
            // No preference at all: identity.
            (None, false),
            (Some(""), false),
            (Some("   "), false),
            (Some(",,,"), false),
            // gzip and its legacy alias, any case.
            (Some("gzip"), true),
            (Some("GZIP"), true),
            (Some("x-gzip"), true),
            (Some("X-Gzip"), true),
            // Weights.
            (Some("gzip;q=1"), true),
            (Some("gzip;q=1.000"), true),
            (Some("gzip;q=1.00"), true),
            (Some("gzip;q=1.0"), true),
            (Some("gzip;q=0"), false),
            (Some("gzip;q=0.000"), false),
            (Some("gzip;q=0.0000"), false),
            (Some("gzip;q=0.001"), true),
            (Some("gzip;q=0.5"), true),
            (Some("gzip;q=0.05"), true),
            (Some("gzip;q=0.005"), true),
            (Some("gzip;q=0.999"), true),
            // Unparsable or out-of-range weights are q=0, never a selection.
            (Some("gzip;q=2"), false),
            (Some("gzip;q=1.5"), false),
            (Some("gzip;q=1.001"), false),
            (Some("gzip;q=-1"), false),
            (Some("gzip;q=bogus"), false),
            (Some("gzip;q="), false),
            (Some("gzip;q"), false),
            (Some("gzip;q=0.1234"), false),
            (Some("gzip;q=0."), false),
            // Unknown parameters are ignored; the first q wins.
            (Some("gzip;level=1"), true),
            (Some("gzip;q=0.5;level=1"), true),
            (Some("gzip;q=0;q=1"), false),
            // OWS around the coding, the name and the value.
            (Some(" gzip ; q = 0.5 "), true),
            // Unsupported codings are parsed and never selected.
            (Some("br, zstd, deflate, compress"), false),
            (Some("br;q=1, gzip;q=0.2"), true),
            // The wildcard stands in for an absent explicit gzip entry…
            (Some("*"), true),
            (Some("*;q=0.5"), true),
            (Some("*;q=0"), false),
            // … and an explicit gzip entry overrides it in both directions.
            (Some("gzip;q=0, *;q=1"), false),
            (Some("gzip;q=1, *;q=0"), true),
            // identity itself never selects gzip.
            (Some("identity"), false),
            (Some("identity;q=0"), false),
            (Some("identity;q=0, *;q=0.5"), true),
            // Duplicate explicit entries: the first one wins.
            (Some("gzip;q=0, gzip;q=1"), false),
            (Some("gzip;q=1, gzip;q=0"), true),
        ];
        for (header, expected) in cases {
            assert_eq!(accepts_gzip(*header), *expected, "{header:?}");
        }
    }

    #[test]
    fn gzip_weights_are_positive_only_inside_the_qvalue_grammar() {
        // The weight decision in its own right: one to three fraction
        // digits, and a nonzero value — `1.000` is weight 1.0, `0.000` is
        // zero, and an out-of-range or malformed weight is never a
        // selection.
        for positive in [
            "1", "1.000", "1.00", "1.0", "0.5", "0.05", "0.005", "0.001", "0.999",
        ] {
            assert!(weight_is_positive(positive), "{positive:?}");
        }
        for zero in [
            "0", "0.000", "0.0000", "2", "1.5", "1.001", "-1", "abc", "", "0.", ".5", "0.1234",
        ] {
            assert!(!weight_is_positive(zero), "{zero:?}");
        }
    }

    #[test]
    fn the_reply_policy_is_decided_from_the_head_it_answers() {
        let head = |method: &str, path: &str, accept_encoding: Option<&str>| {
            let headers: Vec<(&str, &str)> = accept_encoding
                .map(|value| ("accept-encoding", value))
                .into_iter()
                .collect();
            crate::http::tests::head_at(method, path, &headers)
        };
        // The public discovery document is outside the seam: no negotiation,
        // whatever the client advertised.
        assert!(matches!(
            ReplyEncodingPolicy::for_head(&head("GET", "/", Some("gzip")), true),
            ReplyEncodingPolicy::Excluded
        ));
        // Every other reply negotiates from the raw header…
        match ReplyEncodingPolicy::for_head(
            &head("POST", "/db/neo4j/query/v2", Some("gzip")),
            false,
        ) {
            ReplyEncodingPolicy::Negotiated(Some(header)) => assert_eq!(header, "gzip"),
            _ => panic!("a query reply negotiates from its Accept-Encoding"),
        }
        // … including the absent header, which is the identity default.
        match ReplyEncodingPolicy::for_head(&head("GET", "/db/neo4j/query/v2", None), false) {
            ReplyEncodingPolicy::Negotiated(None) => {}
            _ => panic!("a query reply without Accept-Encoding negotiates to identity"),
        }
        // A reply whose head never parsed has no header to read: identity,
        // but still on the seam.
        match ReplyEncodingPolicy::for_unparsed_head() {
            ReplyEncodingPolicy::Negotiated(None) => {}
            _ => panic!("an unparsed head replies identity on the seam"),
        }
    }

    #[test]
    fn the_streaming_negotiation_is_the_weight_decision_alone() {
        // The streaming path cannot run the shrink check (the compressed
        // size is unknown before the head, ADR-0386 §4), so its decision is
        // the shared weight: gzip exactly when the client accepts it.
        assert_eq!(
            BodyEncoding::for_streamed_body(Some("gzip")),
            BodyEncoding::Gzip
        );
        assert_eq!(
            BodyEncoding::for_streamed_body(Some("br, gzip;q=0.5")),
            BodyEncoding::Gzip
        );
        assert_eq!(
            BodyEncoding::for_streamed_body(Some("*")),
            BodyEncoding::Gzip
        );
        assert_eq!(
            BodyEncoding::for_streamed_body(None),
            BodyEncoding::Identity
        );
        assert_eq!(
            BodyEncoding::for_streamed_body(Some("gzip;q=0")),
            BodyEncoding::Identity
        );
        assert_eq!(
            BodyEncoding::for_streamed_body(Some("identity")),
            BodyEncoding::Identity
        );
    }

    #[test]
    fn gzip_applies_only_at_the_threshold_and_only_when_it_shrinks() {
        // One byte below the threshold: identity, bytes untouched.
        let below = vec![b'a'; GZIP_THRESHOLD_BYTES - 1];
        let encoded = encode_body(Some("gzip"), below.clone()).unwrap();
        assert_eq!(encoded.encoding, BodyEncoding::Identity);
        assert_eq!(encoded.bytes, below);

        // Exactly the threshold and compressible: gzip, with a roundtrip.
        let at = vec![b'a'; GZIP_THRESHOLD_BYTES];
        let encoded = encode_body(Some("gzip"), at.clone()).unwrap();
        assert_eq!(encoded.encoding, BodyEncoding::Gzip);
        assert!(encoded.bytes.len() < at.len(), "the gzip body shrinks");
        assert_eq!(encoded.encoding.header_value(), Some("gzip"));
        assert_eq!(gunzip(&encoded.bytes), at);

        // At the threshold but incompressible: the shrink check keeps
        // identity, so a gzip envelope never grows the body.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let incompressible: Vec<u8> = (0..GZIP_THRESHOLD_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let encoded = encode_body(Some("gzip"), incompressible.clone()).unwrap();
        assert_eq!(encoded.encoding, BodyEncoding::Identity);
        assert_eq!(encoded.bytes, incompressible);

        // No gzip advertised: identity at any size.
        let encoded = encode_body(None, vec![b'a'; GZIP_THRESHOLD_BYTES * 4]).unwrap();
        assert_eq!(encoded.encoding, BodyEncoding::Identity);
        assert_eq!(BodyEncoding::Identity.header_value(), None);
    }
}
