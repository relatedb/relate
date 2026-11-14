//! The whole-run request-stream budget both sides of the mount charge
//! their lines against (ADR-0598) — the one accounting the host's
//! session loop and the child kit's writer share, so the two sides count
//! the same bytes under the same framing and neither re-derives the
//! policy or the line cost at its use site.

use super::protocol::CommitFacts;

/// The whole-run request-stream budget one side of the mount charges its
/// lines against (ADR-0598).
///
/// [`Self::under`] is the one construction: the commit policy decides
/// which state exists at all — uncapped for a `per-batch` session (no
/// whole-run accounting: every charge is free, and retained work is
/// bounded by the per-call caps and the store's live-graph limit),
/// capped for a `whole-import` session (the framing that retains every
/// call in one transaction until its single commit, bounded before that
/// commit by the cap and the running charge) — and the derivation lives
/// beside the vocabulary it reads. The state is private: only
/// [`Self::under`] may build a budget and only the charge methods may
/// move it, so the accounting cannot be preset or forked from outside.
///
/// The budget is held by value and charged in place — deliberately not
/// `Copy`: a copied budget would charge independently of the original,
/// and the whole-run accounting it keeps would silently fork.
#[derive(Debug)]
pub struct StreamBudget {
    /// The framing's accounting state — [`Self::under`]'s one match over
    /// the commit policy.
    state: BudgetState,
}

/// A budget's two states, private to the kit: only
/// [`StreamBudget::under`](StreamBudget::under) builds the one the
/// policy frames, and only the charge methods read or move it.
#[derive(Debug)]
enum BudgetState {
    /// A `per-batch` session: no whole-run budget exists, so there is
    /// nothing to charge and nothing to refuse — the per-call caps and
    /// the store's live-graph limit are that session's bounds.
    Uncapped,
    /// A `whole-import` session: the cap its request stream answers to,
    /// and the bytes charged so far — every line that side wrote, a
    /// session's handshake lines included, charged by the child kit's
    /// `start` once the framing is known.
    Capped {
        /// The cap this budget refuses past.
        limit: u64,
        /// The bytes charged so far.
        charged: u64,
    },
}

/// What the budget's charge methods refuse with: the whole-run cap the
/// charge would have crossed (ADR-0598). One typed payload for both
/// sides of the mount — the child kit's and the host's refusals name it
/// in their own words, each converted from this once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamCapExceeded {
    /// The cap the charge would have crossed, in bytes.
    pub limit: u64,
}

impl StreamBudget {
    /// The budget `policy` frames with the session's published stream
    /// cap, starting empty — the one derivation of whether a session's
    /// request stream answers to a whole-run cap at all (ADR-0598),
    /// matched once over the policy's own vocabulary: `whole-import`
    /// framing retains every call in one transaction until its single
    /// commit, so its stream is capped by the published number;
    /// `per-batch` framing retains nothing between calls, so its budget
    /// is uncapped and the per-call caps and the store's live-graph
    /// limit are its bounds.
    pub fn under(policy: CommitFacts, limit: u64) -> Self {
        Self {
            state: match policy {
                CommitFacts::WholeImport => BudgetState::Capped { limit, charged: 0 },
                CommitFacts::PerBatch => BudgetState::Uncapped,
            },
        }
    }

    /// Charge one written request line: its payload bytes plus the
    /// terminator `write_line` appends — the writer's half of the one
    /// line-cost rule (ADR-0598). Refused with the cap before the
    /// charge lands, so the line the refusal answers never ships.
    pub fn charge_written_line(&mut self, line: &str) -> Result<(), StreamCapExceeded> {
        self.charge(line.len() as u64 + 1)
    }

    /// Charge one read request line: it arrives with its terminator
    /// already on it, so its cost is its own length — the reader's half
    /// of the one line-cost rule [`Self::charge_written_line`] owns
    /// (ADR-0598).
    pub fn charge_read_line(&mut self, line: &[u8]) -> Result<(), StreamCapExceeded> {
        self.charge(line.len() as u64)
    }

    /// Charge `cost` bytes. The arithmetic is checked: a sum that
    /// overflows refuses rather than wrapping past the cap. An uncapped
    /// budget charges nothing and refuses nothing.
    fn charge(&mut self, cost: u64) -> Result<(), StreamCapExceeded> {
        match &mut self.state {
            BudgetState::Uncapped => Ok(()),
            BudgetState::Capped { limit, charged } => match charged.checked_add(cost) {
                Some(next) if next <= *limit => {
                    *charged = next;
                    Ok(())
                }
                _ => Err(StreamCapExceeded { limit: *limit }),
            },
        }
    }
}
