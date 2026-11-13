//! PackStream v1: the wire value model of Bolt 4.4 and 5.4 (ADR-0484):
//! the `Pack` model, the byte codec both wire directions share (with the
//! depth and value budgets) and the chunked-message helpers. The
//! engine-value → `Pack` conversion family — the negotiated dialect
//! (`dialect`), the `ValueCodec` and its encoder (`value_codec`) — lives
//! in those leaf modules, beside this one.

#[derive(Debug, Clone, PartialEq)]
pub enum Pack {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    List(Vec<Pack>),
    Map(Vec<(Pack, Pack)>),
    Struct { tag: u8, fields: Vec<Pack> },
    Bytes(Vec<u8>),
}

impl Pack {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Pack::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(Pack, Pack)]> {
        match self {
            Pack::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The value of `key` when `self` is a map; `None` otherwise or when the
    /// key is absent. The single map lookup shared by the protocol handlers.
    pub fn get(&self, key: &str) -> Option<&Pack> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Pack::Int(i) => Some(*i),
            _ => None,
        }
    }
}

pub fn encode(p: &Pack) -> Result<Vec<u8>, String> {
    let mut o = Vec::new();
    enc(p, &mut o, 0)?;
    Ok(o)
}

/// Adapt a codec `String` error into the transport's `io::Error`
/// (`InvalidData`) for the callers that write a message to a stream. It
/// lives next to the codec that owns the `String` error type, so the
/// protocol and failure modules both depend on `packstream` and never on
/// each other.
pub(crate) fn encode_error(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn enc(p: &Pack, o: &mut Vec<u8>, depth: usize) -> Result<(), String> {
    match p {
        Pack::Null => o.push(0xC0),
        Pack::Bool(true) => o.push(0xC3),
        Pack::Bool(false) => o.push(0xC2),
        Pack::Int(i) => enc_int(*i, o),
        Pack::Float(f) => {
            o.push(0xC1);
            o.extend_from_slice(&f.to_be_bytes());
        }
        Pack::String(s) => {
            let b = s.as_bytes();
            enc_len_tiny(o, b.len(), 0x80, 0xD0, 0xD1, 0xD2)?;
            o.extend_from_slice(b);
        }
        Pack::List(xs) => {
            enc_len_tiny(o, xs.len(), 0x90, 0xD4, 0xD5, 0xD6)?;
            let next = child_depth(depth)?;
            for x in xs {
                enc(x, o, next)?;
            }
        }
        Pack::Map(m) => {
            enc_len_tiny(o, m.len(), 0xA0, 0xD8, 0xD9, 0xDA)?;
            let next = child_depth(depth)?;
            for (k, v) in m {
                enc(k, o, next)?;
                enc(v, o, next)?;
            }
        }
        Pack::Struct { tag, fields } => {
            if fields.len() > 15 {
                return Err("packstream struct has more than 15 fields".into());
            }
            o.push(0xB0 | fields.len() as u8);
            o.push(*tag);
            let next = child_depth(depth)?;
            for f in fields {
                enc(f, o, next)?;
            }
        }
        Pack::Bytes(b) => {
            enc_len(o, b.len(), 0xCC, 0xCD, 0xCE)?;
            o.extend_from_slice(b);
        }
    }
    Ok(())
}

/// Write a container's marker and length header for the tiers every
/// container has. `m8`/`m16`/`m32` are the 8-, 16-, and 32-bit markers,
/// each written with its big-endian length; a length beyond `u32` fails
/// through [`wire_len`]. Bytes uses this directly; String, List, and Map
/// use [`enc_len_tiny`], which adds the tiny tier on top.
fn enc_len(o: &mut Vec<u8>, n: usize, m8: u8, m16: u8, m32: u8) -> Result<(), String> {
    if n <= 255 {
        o.push(m8);
        o.push(n as u8);
    } else if n <= 65535 {
        o.push(m16);
        o.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        o.push(m32);
        o.extend_from_slice(&wire_len(n)?.to_be_bytes());
    }
    Ok(())
}

/// Write a String/List/Map header: `tiny` is the tiny marker
/// (`0x80`/`0x90`/`0xA0`), used when `n <= 15`; larger lengths fall through
/// to the shared 8/16/32-bit tier chain in [`enc_len`].
fn enc_len_tiny(
    o: &mut Vec<u8>,
    n: usize,
    tiny: u8,
    m8: u8,
    m16: u8,
    m32: u8,
) -> Result<(), String> {
    if n <= 15 {
        o.push(tiny | n as u8);
        return Ok(());
    }
    enc_len(o, n, m8, m16, m32)
}

fn wire_len(n: usize) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| "packstream length exceeds u32".into())
}

fn enc_int(i: i64, o: &mut Vec<u8>) {
    if (-16..128).contains(&i) {
        o.push(i as u8);
    } else if (i8::MIN as i64..=i8::MAX as i64).contains(&i) {
        o.push(0xC8);
        o.push(i as u8);
    } else if (i16::MIN as i64..=i16::MAX as i64).contains(&i) {
        o.push(0xC9);
        o.extend_from_slice(&(i as i16).to_be_bytes());
    } else if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
        o.push(0xCA);
        o.extend_from_slice(&(i as i32).to_be_bytes());
    } else {
        o.push(0xCB);
        o.extend_from_slice(&i.to_be_bytes());
    }
}

/// Why a message failed to decode. The value budget is its own variant: a
/// well-formed message over the budget is a recoverable request failure the
/// session answers with an actionable FAILURE, while every other decode
/// error is a fatal protocol breach (ADR-0315).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// The message decodes to more than its [`DecodeLimit`] of values:
    /// `at_least_bytes` is a lower bound on the size it decodes to and
    /// `limit` the one it was measured against.
    BudgetExceeded {
        at_least_bytes: usize,
        limit: DecodeLimit,
    },
    /// Any other malformed PackStream input.
    Malformed(String),
}

impl DecodeError {
    /// Whether the error ends the connection (ADR-0315): every malformed
    /// input does; the decode budget alone is a recoverable request
    /// failure (ADR-0577). The one classification, beside the enum, so a
    /// new variant declares its class once.
    pub(crate) fn is_fatal(&self) -> bool {
        match self {
            DecodeError::BudgetExceeded { .. } => false,
            DecodeError::Malformed(_) => true,
        }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BudgetExceeded { .. } => f.write_str("packstream decode budget exceeded"),
            DecodeError::Malformed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<String> for DecodeError {
    fn from(m: String) -> Self {
        DecodeError::Malformed(m)
    }
}

impl From<&str> for DecodeError {
    fn from(m: &str) -> Self {
        DecodeError::Malformed(m.into())
    }
}

/// Decode one message, keeping the failure typed. [`decode`] is the
/// string-error spelling the sibling test and tooling crates use.
pub(crate) fn decode_message(data: &[u8], limit: DecodeLimit) -> Result<Pack, DecodeError> {
    let mut buf = data;
    let mut budget = DecodeBudget::new(limit);
    dec(&mut buf, 0, &mut budget)
}

pub fn decode(data: &[u8]) -> Result<Pack, String> {
    decode_message(data, DecodeLimit::Open).map_err(|e| e.to_string())
}

/// Whether `marker` opens a PackStream structure (`0xB0..=0xBF`); the
/// marker's low nibble is the field count.
fn is_struct_marker(marker: u8) -> bool {
    (0xB0..0xC0).contains(&marker)
}

/// The tag byte of the structure at the start of a raw, not-yet-decoded
/// payload: `Some(tag)` when `data` opens with a structure marker and its
/// tag byte, `None` otherwise. Reading a signature costs two bytes and no
/// allocation, so a batch's messages can be scanned without decoding them
/// (the session's RESET pre-scan). `Some` does not promise the payload
/// decodes: a truncated struct body still fails [`decode`].
pub(crate) fn struct_tag(data: &[u8]) -> Option<u8> {
    match data {
        [marker, tag, ..] if is_struct_marker(*marker) => Some(*tag),
        _ => None,
    }
}

/// The container-depth limit: 128 nested containers, the root counting
/// as 1. Shared by the byte codec's two directions and, through
/// [`child_depth`], the engine-value conversion in `value_codec`.
pub(crate) const MAX_DEPTH: usize = 128;
// Wire bytes alone do not bound the heap occupied by millions of one-byte
// scalar values: each decodes to a whole `Pack` node. The budget counts what
// the decoded message occupies — one `Pack` node per value (the root,
// containers, both halves of each map, which also covers the slot a value
// takes in its parent) plus string and byte payloads — so a dense message of
// small rows fits while a hostile message of millions of scalars does not.
// Lists and maps grow by pushing, so the real allocation can reach twice the
// charged slots; the worst-case heap is therefore about twice the limit.

/// Which per-message decoded-size limit a message is decoded under
/// (ADR-0579): a connection that has not authenticated yet holds only a
/// small one, because an unauthenticated peer can open connections freely
/// and each could otherwise pin the full open-session limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeLimit {
    /// Before the session is open: the handshake's HELLO and LOGON are a
    /// few kilobytes, so 1 MiB costs a legitimate client nothing.
    PreAuth,
    /// The open session: the bulk loads the per-message budget exists to
    /// admit, 64 MiB.
    Open,
}

impl DecodeLimit {
    /// The limit in bytes: the one place either number is spelled.
    pub(crate) const fn bytes(self) -> usize {
        match self {
            DecodeLimit::PreAuth => 1024 * 1024,
            DecodeLimit::Open => 64 * 1024 * 1024,
        }
    }
}

/// One message's decode budget: the limit it started with and what is
/// left of it. Owns every charge so `at_least_bytes` is computed in one
/// place.
struct DecodeBudget {
    limit: DecodeLimit,
    remaining: usize,
}

impl DecodeBudget {
    fn new(limit: DecodeLimit) -> Self {
        Self {
            limit,
            remaining: limit.bytes(),
        }
    }

    /// The refusal for a message now known to decode to at least `extra`
    /// bytes beyond what is already charged: a lower bound, since the
    /// decoder stops here.
    fn exceeded(&self, extra: usize) -> DecodeError {
        DecodeError::BudgetExceeded {
            at_least_bytes: (self.limit.bytes() - self.remaining).saturating_add(extra),
            limit: self.limit,
        }
    }

    /// Refuse `cost` bytes that cannot fit what is left: the one guard every
    /// charge and pre-check reads, so the refusal is spelled once.
    fn refuse_over(&self, cost: usize) -> Result<(), DecodeError> {
        if cost > self.remaining {
            return Err(self.exceeded(cost));
        }
        Ok(())
    }

    /// Take `cost` bytes from the budget, or report the smallest size the
    /// message is now known to decode to.
    fn charge(&mut self, cost: usize) -> Result<(), DecodeError> {
        self.refuse_over(cost)?;
        self.remaining -= cost;
        Ok(())
    }

    /// Refuse a declared count of bare nodes (a list's elements or a
    /// struct's fields) that cannot fit the remaining budget, before any
    /// child is decoded.
    fn check_nodes(&self, n: usize) -> Result<(), DecodeError> {
        self.refuse_over(n.saturating_mul(NODE_BYTES))
    }

    /// [`Self::check_nodes`] for a map's entries: a key node and a value
    /// node each.
    fn check_map_entries(&self, n: usize) -> Result<(), DecodeError> {
        self.refuse_over(n.saturating_mul(2).saturating_mul(NODE_BYTES))
    }
}

/// The charge for one decoded value's own node.
pub(crate) const NODE_BYTES: usize = std::mem::size_of::<Pack>();

/// The one container-depth counting rule, shared by encode and decode so
/// a message built from a `Value` can always be decoded by the peer;
/// `value_codec::child_depth_mdb` adapts its error for the engine-value
/// family. `depth` counts enclosing containers, not scalar leaves — the
/// root container has depth 1, so 128 nested containers are accepted and
/// the 129th is rejected.
pub(crate) fn child_depth(depth: usize) -> Result<usize, String> {
    if depth >= MAX_DEPTH {
        return Err(NESTING_LIMIT_EXCEEDED_TEXT.into());
    }
    Ok(depth + 1)
}

/// The depth-limit failure's one text — `child_depth`'s error string and
/// `MdbPackError::DepthExceeded`'s `Display` (in `value_codec`) render
/// the same words, never a second spelling.
pub(crate) const NESTING_LIMIT_EXCEEDED_TEXT: &str = "packstream nesting limit exceeded";

// Every read advances this borrowed cursor only after the requested bytes exist.
// Callers supply the original field-specific truncated-input error.
fn consume<'a>(buf: &mut &'a [u8], n: usize, short: &'static str) -> Result<&'a [u8], String> {
    if buf.len() < n {
        return Err(short.into());
    }
    let (value, rest) = buf.split_at(n);
    *buf = rest;
    Ok(value)
}

fn read_fixed<const N: usize>(buf: &mut &[u8]) -> Result<[u8; N], String> {
    let mut value = [0; N];
    value.copy_from_slice(consume(buf, N, "short packstream field")?);
    Ok(value)
}

fn read_u8(buf: &mut &[u8]) -> Result<u8, String> {
    Ok(read_fixed::<1>(buf)?[0])
}

fn read_u16(buf: &mut &[u8]) -> Result<usize, String> {
    Ok(u16::from_be_bytes(read_fixed(buf)?) as usize)
}

fn read_u32(buf: &mut &[u8]) -> Result<usize, String> {
    Ok(u32::from_be_bytes(read_fixed(buf)?) as usize)
}

fn dec(buf: &mut &[u8], depth: usize, budget: &mut DecodeBudget) -> Result<Pack, DecodeError> {
    budget.charge(NODE_BYTES)?;
    let marker = consume(buf, 1, "eof")?[0];
    match marker {
        0xC0 => Ok(Pack::Null),
        0xC2 => Ok(Pack::Bool(false)),
        0xC3 => Ok(Pack::Bool(true)),
        0xC1 => Ok(Pack::Float(f64::from_be_bytes(read_fixed(buf)?))),
        n if n < 0x80 => Ok(Pack::Int(n as i64)),
        n if n >= 0xF0 => Ok(Pack::Int((n as i8) as i64)),
        0xC8 => Ok(Pack::Int(i8::from_be_bytes(read_fixed(buf)?) as i64)),
        0xC9 => Ok(Pack::Int(i16::from_be_bytes(read_fixed(buf)?) as i64)),
        0xCA => Ok(Pack::Int(i32::from_be_bytes(read_fixed(buf)?) as i64)),
        0xCB => Ok(Pack::Int(i64::from_be_bytes(read_fixed(buf)?))),
        0xCC => {
            let n = read_u8(buf)? as usize;
            dec_bytes(buf, n, budget)
        }
        0xCD => {
            let n = read_u16(buf)?;
            dec_bytes(buf, n, budget)
        }
        0xCE => {
            let n = read_u32(buf)?;
            dec_bytes(buf, n, budget)
        }
        n if (0x80..0x90).contains(&n) => dec_str(buf, (n & 0x0F) as usize, budget),
        0xD0 => {
            let n = read_u8(buf)? as usize;
            dec_str(buf, n, budget)
        }
        0xD1 => {
            let n = read_u16(buf)?;
            dec_str(buf, n, budget)
        }
        0xD2 => {
            let n = read_u32(buf)?;
            dec_str(buf, n, budget)
        }
        n if (0x90..0xA0).contains(&n) => dec_list(buf, (n & 0x0F) as usize, depth, budget),
        0xD4 => {
            let n = read_u8(buf)? as usize;
            dec_list(buf, n, depth, budget)
        }
        0xD5 => {
            let n = read_u16(buf)?;
            dec_list(buf, n, depth, budget)
        }
        0xD6 => {
            let n = read_u32(buf)?;
            dec_list(buf, n, depth, budget)
        }
        n if (0xA0..0xB0).contains(&n) => dec_map(buf, (n & 0x0F) as usize, depth, budget),
        0xD8 => {
            let n = read_u8(buf)? as usize;
            dec_map(buf, n, depth, budget)
        }
        0xD9 => {
            let n = read_u16(buf)?;
            dec_map(buf, n, depth, budget)
        }
        0xDA => {
            let n = read_u32(buf)?;
            dec_map(buf, n, depth, budget)
        }
        n if is_struct_marker(n) => {
            let nfields = (n & 0x0F) as usize;
            let tag = read_u8(buf)?;
            let next_depth = child_depth(depth)?;
            if nfields > buf.len() {
                return Err("short packstream struct".into());
            }
            budget.check_nodes(nfields)?;
            // A field count is wire data: allocate only after a field decodes.
            let mut fields = Vec::new();
            for _ in 0..nfields {
                fields.push(dec(buf, next_depth, budget)?);
            }
            Ok(Pack::Struct { tag, fields })
        }
        _ => Err(format!("unknown packstream marker {marker:#x}").into()),
    }
}

fn dec_str(buf: &mut &[u8], n: usize, budget: &mut DecodeBudget) -> Result<Pack, DecodeError> {
    let s = consume(buf, n, "short string")?;
    budget.charge(n)?;
    Ok(Pack::String(
        std::str::from_utf8(s)
            .map_err(|e| e.to_string())?
            .to_owned(),
    ))
}

fn dec_bytes(buf: &mut &[u8], n: usize, budget: &mut DecodeBudget) -> Result<Pack, DecodeError> {
    let b = consume(buf, n, "short bytes")?;
    budget.charge(n)?;
    Ok(Pack::Bytes(b.to_vec()))
}

fn dec_list(
    buf: &mut &[u8],
    n: usize,
    depth: usize,
    budget: &mut DecodeBudget,
) -> Result<Pack, DecodeError> {
    let next_depth = child_depth(depth)?;
    if n > buf.len() {
        return Err("short packstream list".into());
    }
    budget.check_nodes(n)?;
    // The declared count can be large even when a nested child later fails.
    let mut xs = Vec::new();
    for _ in 0..n {
        xs.push(dec(buf, next_depth, budget)?);
    }
    Ok(Pack::List(xs))
}

fn dec_map(
    buf: &mut &[u8],
    n: usize,
    depth: usize,
    budget: &mut DecodeBudget,
) -> Result<Pack, DecodeError> {
    let next_depth = child_depth(depth)?;
    if n > buf.len() / 2 {
        return Err("short packstream map".into());
    }
    budget.check_map_entries(n)?;
    let mut m = Vec::new();
    for _ in 0..n {
        let k = dec(buf, next_depth, budget)?;
        let v = dec(buf, next_depth, budget)?;
        m.push((k, v));
    }
    Ok(Pack::Map(m))
}

pub fn chunk_message(payload: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        let n = (payload.len() - i).min(65535);
        o.extend_from_slice(&(n as u16).to_be_bytes());
        o.extend_from_slice(&payload[i..i + n]);
        i += n;
    }
    o.extend_from_slice(&0u16.to_be_bytes());
    o
}

pub fn try_struct_msg(tag: u8, fields: Vec<Pack>) -> Result<Vec<u8>, String> {
    Ok(chunk_message(&encode(&Pack::Struct { tag, fields })?))
}

// Existing test clients build only fixed, valid Bolt messages.
#[cfg(test)]
pub fn struct_msg(tag: u8, fields: Vec<Pack>) -> Vec<u8> {
    try_struct_msg(tag, fields).expect("test Bolt message is valid")
}

#[cfg(test)]
#[path = "packstream_tests.rs"]
mod tests;
