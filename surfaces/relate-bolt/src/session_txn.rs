//! The transaction-lifecycle leg of the session (ADR-0028's protocol
//! shape): BEGIN, COMMIT and ROLLBACK — the handlers behind those arms of
//! `Session`'s dispatch — beside the transaction-scoped session state they
//! open and end (ADR-0311, ADR-0484 decision 8). A child of `session`, so
//! the handlers keep touching the session's private state; the gates that
//! route messages here live in the parent.

use super::*;

impl Session {
    /// BEGIN (0x11). ADR-0117 §7: BEGIN selects the transaction's namespace
    /// from its extra's `db`, applied before `begin()` because
    /// `set_namespace` fails while a transaction is open (a BEGIN inside
    /// one is a breach and never reaches here, ADR-0314). The extra's
    /// `bookmarks` are accepted and ignored (ADR-0310). At 5.4 the extra's
    /// notifications keys (ADR-0484 decision 8) overlay the HELLO config
    /// for the transaction's statements — an invalid value fails the
    /// BEGIN before anything starts; at 4.4 the keys are ignored and the
    /// connection config stands. ADR-0610: the extra's `tx_metadata`
    /// seeds the connection's per-transaction metadata map while the
    /// `value-provenance` opt-in is set — a malformed value fails the
    /// BEGIN before anything starts, and the seeding itself can fail only
    /// on the metadata envelope (surfaced as the BEGIN's engine failure);
    /// it runs **before** the transaction opens — the cell needs no open
    /// transaction — so an over-envelope seed fails the BEGIN leaving no
    /// open transaction behind, the same fail-before-start rule the
    /// extra's other keys follow. Without the opt-in the key is ignored,
    /// exactly as before the feature. The transaction starts with no open
    /// results and qids from 0 (ADR-0311).
    pub(super) fn begin<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
    ) -> std::io::Result<Flow> {
        let tx_notifications = match self.overlay_notifications(fields, 0, &self.notifications) {
            Ok(config) => config,
            Err(parse_failure) => return self.fail_metadata_notifications(stream, &parse_failure),
        };
        let tx_metadata = match begin_tx_metadata(fields, 0, self.dialect) {
            Ok(metadata) => metadata,
            Err(message) => return self.fail_with_code(stream, REQUEST_INVALID_CODE, &message),
        };
        if let Err(e) = apply_extra_db(&mut self.conn, fields, 0) {
            return self.fail_engine(stream, &e);
        }
        if let Some(metadata) = tx_metadata {
            if let Err(e) = self.conn.seed_tx_metadata(metadata) {
                return self.fail_engine(stream, &e);
            }
        }
        if let Err(e) = self.conn.begin() {
            return self.fail_engine(stream, &e);
        }
        self.begin_tx(tx_notifications);
        send_success(stream, vec![])?;
        Ok(Flow::Continue)
    }

    /// Open the transaction-scoped session state (ADR-0311, ADR-0484
    /// decision 8): the notifications config the transaction's
    /// statements read — the BEGIN keys' selection, the connection
    /// config itself at 4.4, where the keys are ignored — and the empty
    /// result store a transaction starts with. [`Self::end_tx`] is the
    /// mirrored end every transaction-ending path shares.
    pub(super) fn begin_tx(&mut self, notifications: NotificationConfig) {
        self.tx_notifications = notifications;
        self.results.clear();
    }

    /// End the transaction-scoped session state together: every open
    /// result (ADR-0311) and the notifications selection (ADR-0484
    /// decision 8), restored to the connection config — the one end
    /// COMMIT, ROLLBACK and RESET call, so a transaction-ending path
    /// cannot drift.
    pub(super) fn end_tx(&mut self) {
        self.results.clear();
        self.tx_notifications = self.notifications.clone();
    }

    /// COMMIT (0x12): one WAL commit under the NOOP keep-alive (ADR-0312);
    /// SUCCESS carries the bookmark the engine's commit returned — the
    /// store's commit sequence (ADR-0310, ADR-0502 §7) — or is `{}` when
    /// the transaction committed nothing. Every open result is dropped
    /// (ADR-0311), and the transaction's notifications selection ends
    /// with it (ADR-0484 decision 8).
    pub(super) fn commit<W: Write>(
        &mut self,
        stream: &mut W,
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        self.end_tx();
        let conn = &mut self.conn;
        let committed = with_keep_alive(stream, config.limits.keep_alive_interval, |heartbeat| {
            conn.commit_with_heartbeat(heartbeat)
        })?;
        let bookmark = match committed {
            Ok(bookmark) => bookmark,
            Err(e) => return self.fail_engine(stream, &e),
        };
        send_success(stream, summary::commit(bookmark))?;
        Ok(Flow::Continue)
    }

    /// ROLLBACK (0x13): discard the transaction's writes and every open
    /// result (ADR-0311), and the transaction's notifications selection
    /// with them (ADR-0484 decision 8).
    pub(super) fn rollback<W: Write>(&mut self, stream: &mut W) -> std::io::Result<Flow> {
        self.end_tx();
        if let Err(e) = self.conn.rollback() {
            return self.fail_engine(stream, &e);
        }
        send_success(stream, vec![])?;
        Ok(Flow::Continue)
    }
}
