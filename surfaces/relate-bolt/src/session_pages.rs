//! The PULL/DISCARD page-writing machinery (ADR-0309, ADR-0110): one
//! step's outcome ([`Page`]), the typed RECORD-write failure
//! ([`RecordError`]) and the writers that encode a result's rows under
//! the connection's value codec — split from `session` at the
//! thousand-line rule, the way the 5.x leg was. A child of that module,
//! so the writers stay beside their only caller, `Session::consume`.

use super::*;

/// One PULL/DISCARD step's outcome (ADR-0309, ADR-0110): the ending SUCCESS
/// when the step took the result's last row, `has_more` when rows remain,
/// the client-reachable depth failure, or a streamed statement's end in
/// failure.
pub(super) enum Page {
    /// The result was consumed to its end: send this ending SUCCESS.
    Ending(summary::Metadata),
    /// Rows remain: restore the result and answer `has_more`.
    HasMore,
    /// A PULL row nested past the packstream depth limit: restore the
    /// result and fail the session (ADR-0110).
    DepthExceeded,
    /// A streamed statement failed while executing (ADR-0319, ADR-0682):
    /// the rows before the failure are written, the FAILURE follows and
    /// the result closes.
    Failed(relate::Error),
    /// A streamed statement's worker waited out the PULL idle timeout and
    /// stopped (ADR-0682): the FAILURE follows and the result closes.
    Stalled,
    /// A streamed batch exceeds the `--max-result-bytes` cap: the FAILURE
    /// follows and the result closes.
    OverCap,
}

/// Encode and write one RECORD (0x71): the one place a result row is
/// encoded, under the connection's value codec (ADR-0329, ADR-0484). The
/// result store (`results.rs`) stays wire-free; the write lives in this
/// child module, beside its only caller, `Session::consume`.
fn write_record<W: Write>(
    stream: &mut W,
    row: &[Value],
    codec: ValueCodec,
) -> Result<(), RecordError> {
    let fields = match row
        .iter()
        .map(|v| Pack::from_mdb(v, codec))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(fields) => fields,
        // The depth limit is client-reachable (ADR-0110): it degrades the
        // session, after any RECORDs already written.
        Err(e) if e.client_reachable() => return Err(RecordError::DepthExceeded),
        // A path-invariant break, an unrepresentable value or an element
        // with no element id is an engine bug or an engine-side value
        // problem, never client input: keep it loud as the io error.
        // The partition itself is `MdbPackError::client_reachable`'s, at
        // the error definition.
        Err(e) => return Err(RecordError::Fatal(encode_error(e.to_string()))),
    };
    let record = Pack::Struct {
        tag: 0x71,
        fields: vec![Pack::List(fields)],
    };
    let framed = chunk_message(
        &encode(&record)
            .map_err(encode_error)
            .map_err(RecordError::Fatal)?,
    );
    stream.write_all(&framed).map_err(RecordError::Fatal)
}

/// A RECORD write's failure, typed so the PULL loop can tell the two apart
/// instead of string-matching: the packstream container-depth limit is
/// client-reachable (a deeply nested parameter value returned in a row), so
/// it degrades the session (ADR-0110); anything else is an io/engine
/// contract break, kept loud.
pub(super) enum RecordError {
    /// A row nested past the packstream depth limit (128).
    DepthExceeded,
    /// An engine contract break or a wire-write failure.
    Fatal(std::io::Error),
}

/// Write one page of RECORDs, stopping at the first failure: the caller
/// maps [`RecordError::DepthExceeded`] to the ADR-0110 session failure and
/// keeps a fatal error loud.
pub(super) fn write_page<W: Write>(
    stream: &mut W,
    rows: &[Vec<Value>],
    codec: ValueCodec,
) -> Result<(), RecordError> {
    for row in rows {
        write_record(stream, row, codec)?;
    }
    Ok(())
}
