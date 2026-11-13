//! One Bolt session: the handshake, the version's auth shape, one statement
//! per call, and the pipelined RUN+PULL outbox the imports drain.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use relate_bolt::packstream::{try_struct_msg, Pack};

use super::messages::{
    describe, hello_entries, int_params, is_success, logon_entries, message_tag, pull_fields,
    record_int, run_fields, run_pull_frame, tag,
};
use super::version::{BoltVersion, MAGIC, PROPOSALS};

/// The default socket read timeout: one statement can build hundreds of
/// thousands of relationships before the server writes a byte back.
pub(super) const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(1800);

pub struct Session {
    stream: TcpStream,
    /// The version the handshake negotiated (ADR-0484): the HELLO and LOGON
    /// auth shapes branch on it.
    version: BoltVersion,
    /// How long one read waits for the server's next byte: one shape statement
    /// can build hundreds of thousands of relationships before the server
    /// writes a byte back.
    read_timeout: Duration,
    /// Messages queued by [`Session::enqueue_run_pull`], written together by
    /// the batch's one [`Session::flush`].
    outbox: Vec<u8>,
}

/// The responses of one drained pipelined batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDrain {
    /// Pairs whose RUN and PULL both answered SUCCESS.
    pub applied: usize,
    /// Pairs the server rejected: the one that answered FAILURE plus every
    /// pair it answered IGNORED behind it.
    pub errors: usize,
    /// The FAILURE text of the batch's first rejected pair.
    pub failure: Option<String>,
}

impl BatchDrain {
    fn new() -> Self {
        Self {
            applied: 0,
            errors: 0,
            failure: None,
        }
    }

    /// Record one rejected pair, keeping the batch's first FAILURE text.
    fn reject(&mut self, msg: &[u8]) {
        self.errors += 1;
        if self.failure.is_none() {
            self.failure = Some(describe(msg));
        }
    }
}

impl Session {
    pub fn connect(addr: &str) -> Result<Self, String> {
        Self::connect_with_auth(addr, None)
    }

    /// The version the handshake negotiated (ADR-0484).
    pub fn version(&self) -> BoltVersion {
        self.version
    }

    /// [`Session::connect`] with basic credentials; `None` is the anonymous
    /// connect every spawned-server caller makes. The credentials ride the 4.4
    /// HELLO map or, at 5.x, the LOGON token (ADR-0126, ADR-0484
    /// decision 4).
    pub fn connect_with_auth(addr: &str, auth: Option<(&str, &str)>) -> Result<Self, String> {
        Self::connect_with_read_timeout(addr, auth, DEFAULT_READ_TIMEOUT)
    }

    /// The one construction path: the handshake every session walks, with
    /// the caller's socket read timeout. Production callers take
    /// [`Self::connect_with_auth`]'s default; the tests pass their own
    /// short windows.
    fn connect_with_read_timeout(
        addr: &str,
        auth: Option<(&str, &str)>,
        read_timeout: Duration,
    ) -> Result<Self, String> {
        let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .map_err(|e| e.to_string())?;
        stream.write_all(&MAGIC).map_err(|e| e.to_string())?;
        stream
            .write_all(PROPOSALS.as_flattened())
            .map_err(|e| e.to_string())?;
        let mut reply = [0u8; 4];
        stream
            .read_exact(&mut reply)
            .map_err(|e| format!("handshake: {e}"))?;
        let mut session = Self {
            stream,
            version: BoltVersion::from_reply(reply)?,
            read_timeout,
            outbox: Vec::new(),
        };
        session.open(auth)?;
        Ok(session)
    }

    /// One message through the crate's canonical reader, which skips the
    /// server's keep-alive NOOP chunks (ADR-0312, ADR-0316): a long shape
    /// statement gets them while it runs. A read that hits the session's read
    /// timeout is the authored timeout text, not the platform's socket error:
    /// the statement produced no byte for longer than the run allows.
    fn read_message(&mut self) -> Result<Vec<u8>, String> {
        relate_bolt::protocol::read_message(&mut self.stream).map_err(|error| match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => format!(
                "the server sent no reply within the {:?} read timeout",
                self.read_timeout
            ),
            _ => format!("read message: {error}"),
        })
    }

    /// One response the failed state answered for work queued behind a FAILURE.
    fn read_ignored(&mut self) -> Result<(), String> {
        let msg = self.read_message()?;
        match message_tag(&msg) {
            Some(tag::IGNORED) => Ok(()),
            _ => Err(format!("expected IGNORED, got {}", describe(&msg))),
        }
    }

    /// Open the session the way the negotiated version authenticates: the
    /// HELLO, then the 5.x LOGON (ADR-0484 decisions 3 and 4).
    fn open(&mut self, auth: Option<(&str, &str)>) -> Result<(), String> {
        self.send(
            tag::HELLO,
            vec![Pack::Map(hello_entries(self.version, auth))],
        )?;
        let hello = self.read_message()?;
        if !is_success(&hello) {
            return Err(format!("HELLO failed: {}", describe(&hello)));
        }
        if !self.version.speaks_5x() {
            return Ok(());
        }
        // Bolt 5.1 moved the credential check out of the HELLO: the session
        // stays in the AUTHENTICATION state until LOGON opens it.
        self.send(tag::LOGON, vec![Pack::Map(logon_entries(auth))])?;
        let logon = self.read_message()?;
        if !is_success(&logon) {
            return Err(format!("LOGON failed: {}", describe(&logon)));
        }
        Ok(())
    }

    pub fn run(&mut self, cypher: &str) -> Result<(), String> {
        self.run_one(cypher, Vec::new()).map(|_| ())
    }

    /// Run one statement with an integer parameter map and discard its rows.
    pub fn run_with_params(&mut self, cypher: &str, params: &[(&str, i64)]) -> Result<(), String> {
        self.run_one(cypher, int_params(params)).map(|_| ())
    }

    /// Run one statement with an integer parameter map and return how many
    /// RECORD messages it produced — the benchmark's per-iteration call, where
    /// the row count is reported and the records themselves are drained.
    pub fn run_records_with_params(
        &mut self,
        cypher: &str,
        params: &[(&str, i64)],
    ) -> Result<usize, String> {
        self.run_one(cypher, int_params(params))
    }

    /// Run one statement and return how many RECORD messages it produced.
    pub fn run_records(&mut self, cypher: &str) -> Result<usize, String> {
        self.run_one(cypher, Vec::new())
    }

    /// Run one statement whose result is one row with one integer — the
    /// bench's `RETURN count(…)` probes — and return that integer.
    pub fn run_count(&mut self, cypher: &str) -> Result<i64, String> {
        let mut count = None;
        let records = self.run_each(cypher, Vec::new(), |record| {
            count = count.or_else(|| record_int(record));
        })?;
        match (records, count) {
            (1, Some(count)) => Ok(count),
            (1, None) => Err(format!(
                "{cypher}: the single row is not one integer column"
            )),
            _ => Err(format!(
                "{cypher}: expected one row with one integer, got {records} rows"
            )),
        }
    }

    /// One statement, one round trip per message: RUN answered, then PULL.
    fn run_one(&mut self, cypher: &str, params: Vec<(Pack, Pack)>) -> Result<usize, String> {
        self.run_each(cypher, params, |_| {})
    }

    /// The one RUN/PULL response loop: one statement, every RECORD handed to
    /// `on_record` as its raw message, and the number of records read.
    fn run_each(
        &mut self,
        cypher: &str,
        params: Vec<(Pack, Pack)>,
        mut on_record: impl FnMut(&[u8]),
    ) -> Result<usize, String> {
        self.send(tag::RUN, run_fields(cypher, params))?;
        let run = self.read_message()?;
        if !is_success(&run) {
            let err = format!("RUN failed: {} — {cypher}", describe(&run));
            self.reset_after_failure();
            return Err(err);
        }
        self.send(tag::PULL, pull_fields())?;
        let mut records = 0usize;
        loop {
            let msg = self.read_message()?;
            match message_tag(&msg) {
                Some(tag::RECORD) => {
                    records += 1;
                    on_record(&msg);
                }
                Some(tag::SUCCESS) => return Ok(records),
                _ => {
                    let err = format!("PULL failed: {} — {cypher}", describe(&msg));
                    self.reset_after_failure();
                    return Err(err);
                }
            }
        }
    }

    /// Queue one RUN+PULL pair without writing it; the batch's one
    /// [`Session::flush`] writes every queued pair.
    pub fn enqueue_run_pull(&mut self, cypher: &str, params: &[(&str, i64)]) -> Result<(), String> {
        self.enqueue_run_pull_pack(cypher, int_params(params))
    }

    /// [`Session::enqueue_run_pull`] with a packstream parameter map, for the
    /// release loader's `UNWIND` batches.
    pub fn enqueue_run_pull_pack(
        &mut self,
        cypher: &str,
        params: Vec<(Pack, Pack)>,
    ) -> Result<(), String> {
        self.outbox.extend(run_pull_frame(cypher, params)?);
        Ok(())
    }

    /// Write every message queued since the last flush as one write.
    pub fn flush(&mut self) -> Result<(), String> {
        let outbox = std::mem::take(&mut self.outbox);
        self.stream.write_all(&outbox).map_err(|e| e.to_string())
    }

    /// Drain the responses of `count` pipelined RUN+PULL pairs queued by
    /// [`Session::enqueue_run_pull`] and written by [`Session::flush`].
    ///
    /// A FAILURE leaves the server in its failed state: every pair still
    /// queued behind it answers IGNORED, and one RESET at the end of the
    /// drain recovers the session for the next batch.
    pub fn drain_batch(&mut self, count: usize) -> Result<BatchDrain, String> {
        let mut drain = BatchDrain::new();
        for _ in 0..count {
            if drain.failure.is_some() {
                // The failed state answers both messages of every pair still
                // queued behind the failure with IGNORED.
                self.read_ignored()?;
                self.read_ignored()?;
                drain.errors += 1;
                continue;
            }
            let run = self.read_message()?;
            match message_tag(&run) {
                Some(tag::SUCCESS) => {}
                Some(tag::FAILURE | tag::IGNORED) => {
                    // This pair's PULL is answered along with its RUN.
                    self.read_ignored()?;
                    drain.reject(&run);
                    continue;
                }
                _ => {
                    return Err(format!(
                        "expected SUCCESS or FAILURE for RUN, got {}",
                        describe(&run)
                    ))
                }
            }
            loop {
                let msg = self.read_message()?;
                match message_tag(&msg) {
                    Some(tag::SUCCESS) => {
                        drain.applied += 1;
                        break;
                    }
                    Some(tag::RECORD) => {}
                    Some(tag::FAILURE | tag::IGNORED) => {
                        drain.reject(&msg);
                        break;
                    }
                    _ => return Err(format!("expected a PULL response, got {}", describe(&msg))),
                }
            }
        }
        if drain.failure.is_some() {
            self.reset_after_failure();
        }
        Ok(drain)
    }

    /// RESET, best effort: it clears the failed state so a later RUN is not
    /// IGNORED, but the caller is already on its way out with the statement
    /// failure that led here — that error is what it reports, never a RESET
    /// that could not be sent or answered on the same broken connection.
    fn reset_after_failure(&mut self) {
        if self.send(tag::RESET, vec![]).is_ok() {
            let _ = self.read_message();
        }
    }

    pub fn goodbye(mut self) -> Result<(), String> {
        self.send(tag::GOODBYE, vec![])
    }

    /// Write one message now — the single-statement path, which owns its own
    /// response and never rides the queued pipelined outbox, so it writes the
    /// frame straight to the stream instead of through [`Session::flush`].
    fn send(&mut self, message: u8, fields: Vec<Pack>) -> Result<(), String> {
        let frame = try_struct_msg(message, fields)?;
        self.stream.write_all(&frame).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::bolt::{field, try_read_msg};

    use super::super::messages::USER_AGENT;
    use super::*;

    /// What one fake-server connection saw: the client's four handshake
    /// proposal slots and every message it sent after them.
    struct Seen {
        proposals: [u8; 16],
        messages: Vec<Pack>,
    }

    /// A one-connection fake Bolt server: answers the handshake with `reply`,
    /// then every message up to GOODBYE with SUCCESS `{}` — the 4.4-only
    /// server no live fixture provides, since the spawned `relate serve`
    /// speaks both versions.
    fn fake_server(reply: [u8; 4]) -> (String, std::thread::JoinHandle<Seen>) {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut magic = [0u8; 4];
            stream.read_exact(&mut magic).unwrap();
            assert_eq!(magic, MAGIC, "the client opens with the Bolt magic");
            let mut proposals = [0u8; 16];
            stream.read_exact(&mut proposals).unwrap();
            stream.write_all(&reply).unwrap();
            let success = try_struct_msg(tag::SUCCESS, vec![Pack::Map(vec![])]).unwrap();
            let mut messages = Vec::new();
            while let Ok(msg) = try_read_msg(&mut stream) {
                let goodbye = matches!(
                    msg,
                    Pack::Struct {
                        tag: tag::GOODBYE,
                        ..
                    }
                );
                messages.push(msg);
                // GOODBYE is answered by the close, never a SUCCESS.
                if goodbye || stream.write_all(&success).is_err() {
                    break;
                }
            }
            Seen {
                proposals,
                messages,
            }
        });
        (addr, handle)
    }

    /// One sent message's signature and its single field — the HELLO and
    /// LOGON exchanges carry exactly one extra map.
    fn sent(msg: &Pack) -> (u8, &Pack) {
        match msg {
            Pack::Struct { tag, fields } if fields.len() == 1 => (*tag, &fields[0]),
            other => panic!("expected a one-field struct, got {other:?}"),
        }
    }

    /// The HELLO and LOGON maps one fake-server session received, in order.
    fn auth_maps(seen: &Seen) -> (&Pack, &Pack) {
        assert_eq!(seen.messages.len(), 3, "HELLO, LOGON and GOODBYE");
        let (hello_tag, hello) = sent(&seen.messages[0]);
        assert_eq!(hello_tag, tag::HELLO);
        assert!(
            matches!(
                seen.messages[2],
                Pack::Struct {
                    tag: tag::GOODBYE,
                    ..
                }
            ),
            "the client says GOODBYE"
        );
        let (logon_tag, logon) = sent(&seen.messages[1]);
        assert_eq!(logon_tag, tag::LOGON);
        (hello, logon)
    }

    /// ADR-0484 decision 2: the handshake proposes 5.4 first and 4.4 second,
    /// and a 5.4 reply opens the 5.x auth flow — the HELLO carries
    /// `user_agent` and `bolt_agent` only, and the anonymous connect's LOGON
    /// sends the `none` scheme.
    #[test]
    fn connect_proposes_5_4_first_and_logs_on_with_none() {
        let (addr, server) = fake_server([0, 0, 4, 5]);
        let session = Session::connect(&addr).unwrap();
        assert_eq!(session.version(), BoltVersion::V5_4);
        session.goodbye().unwrap();

        let seen = server.join().unwrap();
        assert_eq!(
            seen.proposals,
            [0, 0, 4, 5, 0, 0, 4, 4, 0, 0, 0, 0, 0, 0, 0, 0],
            "Bolt 5.4, then the 4.4 fallback, then two empty slots"
        );
        let (hello, logon) = auth_maps(&seen);
        assert_eq!(field(hello, "user_agent"), USER_AGENT);
        assert!(
            hello.get("bolt_agent").is_some(),
            "a 5.x HELLO carries the required bolt_agent map"
        );
        for auth_key in ["scheme", "principal", "credentials"] {
            assert!(
                hello.get(auth_key).is_none(),
                "the 5.x HELLO does not authenticate ({auth_key})"
            );
        }
        assert_eq!(field(logon, "scheme"), "none");
    }

    /// ADR-0484 decisions 3 and 4: connect mode's credentials are not in the
    /// 5.x HELLO either — they ride the LOGON's `basic` token.
    #[test]
    fn connect_with_auth_logs_on_with_the_basic_token() {
        let (addr, server) = fake_server([0, 0, 4, 5]);
        let session = Session::connect_with_auth(&addr, Some(("relate", "token"))).unwrap();
        assert_eq!(session.version(), BoltVersion::V5_4);
        session.goodbye().unwrap();

        let seen = server.join().unwrap();
        let (hello, logon) = auth_maps(&seen);
        assert!(
            hello.get("credentials").is_none(),
            "5.x LOGON carries the credentials, not the HELLO"
        );
        assert_eq!(field(logon, "scheme"), "basic");
        assert_eq!(field(logon, "principal"), "relate");
        assert_eq!(field(logon, "credentials"), "token");
    }

    /// The 4.4 fallback: a server answering 4.4 gets the 4.4 shape — the
    /// credentials in the HELLO map and no LOGON at all (ADR-0126).
    #[test]
    fn connect_falls_back_to_4_4_and_carries_the_credentials_in_the_hello() {
        let (addr, server) = fake_server([0, 0, 4, 4]);
        let session = Session::connect_with_auth(&addr, Some(("relate", "token"))).unwrap();
        assert_eq!(session.version(), BoltVersion::V4_4);
        session.goodbye().unwrap();

        let seen = server.join().unwrap();
        assert_eq!(
            seen.messages.len(),
            2,
            "a 4.4 exchange is HELLO then GOODBYE"
        );
        let (_, hello) = sent(&seen.messages[0]);
        assert_eq!(field(hello, "scheme"), "basic");
        assert_eq!(field(hello, "principal"), "relate");
        assert_eq!(field(hello, "credentials"), "token");
        assert!(
            hello.get("bolt_agent").is_none(),
            "bolt_agent is a 5.x HELLO key"
        );
    }

    /// A one-connection fake server that answers the handshake and the HELLO,
    /// swallows the client's statement and never answers it: only the client's
    /// read timeout (or its close) ends the wait.
    fn stalled_server() -> (String, std::thread::JoinHandle<()>) {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut magic = [0u8; 4];
            stream.read_exact(&mut magic).unwrap();
            let mut proposals = [0u8; 16];
            stream.read_exact(&mut proposals).unwrap();
            stream.write_all(&[0, 0, 4, 4]).unwrap();
            let success = try_struct_msg(tag::SUCCESS, vec![Pack::Map(vec![])]).unwrap();
            try_read_msg(&mut stream).unwrap();
            stream.write_all(&success).unwrap();
            // The statement (RUN) is read and left unanswered; the client's
            // close is what ends this thread.
            try_read_msg(&mut stream).unwrap();
            let mut byte = [0u8; 1];
            let _ = stream.read(&mut byte);
        });
        (addr, handle)
    }

    /// ADR-0495: a statement whose server never answers fails with the
    /// authored timeout text — the platform's socket error is not what the
    /// run reports.
    #[test]
    fn a_stalled_statement_fails_with_the_authored_timeout() {
        let (addr, server) = stalled_server();
        let mut session =
            Session::connect_with_read_timeout(&addr, None, Duration::from_millis(250)).unwrap();
        let error = session.run_records("RETURN 1").unwrap_err();
        assert!(
            error.contains("read timeout") && error.contains("250ms"),
            "want the authored timeout naming the duration, got {error:?}"
        );
        // Closing the session is the EOF the stalled server waits for.
        drop(session);
        server.join().unwrap();
    }
}
