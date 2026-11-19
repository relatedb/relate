//! The comprehension-WHERE `|` disambiguation — one Parser state family:
//! the context's bracket proof ([`BracketProof`], the value of the
//! `comprehension_where` field on the parser core), its one scoped
//! lifecycle owner ([`Parser::with_comprehension_where`]), and the one
//! commit-on-success mechanism both `|`-separated grammars resolve the
//! ambiguity through ([`Parser::ambiguous_pipe_continuation`], with the
//! grammar-specific tail classifications living beside their grammars) —
//! split out of `mod.rs` the way `speculation.rs` and `side_effects.rs`
//! hold their own state families, so the parser core's field list stays
//! grammar-only.

use super::Parser;
use crate::error::ParseResult;
use crate::token::TokenType;

/// The `]`-proof a comprehension-WHERE context carries — the one fact
/// the label side's tail classification reads: whether a `]` right after
/// a would-be pipe continuation proves the `|` was the comprehension's
/// map separator. Each comprehension form passes the variant naming its
/// own `]`-situation.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum BracketProof {
    /// The pattern form: the comprehension's own closer is `]` and its
    /// map expression is required, so the `]` would close the
    /// comprehension with the map stranded — the proof holds.
    RightBracketClosesTheComprehension,
    /// The list form: the comprehension's own closer is `]` but its map
    /// expression is optional, so the `]` closes the comprehension
    /// legally with the map omitted — no proof.
    RightBracketClosesWithTheMapOmitted,
    /// The CIP-272 map form: the comprehension's closer is `}`, so every
    /// `]` inside it belongs to an inner bracket — no proof.
    InnerBracketOnly,
}

impl<'a> Parser<'a> {
    /// Sets the comprehension-WHERE context for the whole of `f` — the
    /// one lifecycle owner of the parser's `comprehension_where` field:
    /// the comprehension parsers enter a context through
    /// [`Parser::parse_comprehension_where_expr`], the value-type
    /// brackets suspend it (`None`) through
    /// [`Parser::parse_bracketed_value_type`], and no other site touches
    /// the field. Balanced by construction like the depth guards
    /// (`depth_guard_family!`'s `with`): `f`'s result is captured, the
    /// previous context is restored, and only then is the result
    /// returned, so no `?` can ever sit between enter and exit — an
    /// erroring WHERE parse can never leak its context into the
    /// enclosing expression, and nested comprehension WHEREs each
    /// restore their enclosing context.
    pub(super) fn with_comprehension_where<T>(
        &mut self,
        context: Option<BracketProof>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let saved = self.comprehension_where;
        self.comprehension_where = context;
        let result = f(self);
        self.comprehension_where = saved;
        result
    }

    /// The one commit-on-success mechanism for the comprehension-WHERE
    /// `|` ambiguity (ADR-0626's reader-driven shape without a probe),
    /// shared by the label disjunction (`parse_label_or`) and the
    /// `<value type>` union (`parse_value_type_at_current_depth`):
    /// snapshot the position and every side-effect flag, consume the
    /// `|`, parse the would-be continuation once through `parse`, and
    /// classify the token after it through the caller's
    /// `tail_ends_where`. A real continuation is returned unchanged
    /// (`Some`); a non-continuation inside a comprehension-WHERE context
    /// — the continuation failed to parse, or `tail_ends_where` proves
    /// the parsed chain was really the comprehension's map expression —
    /// restores the snapshot and yields `None`: the `|` is the
    /// comprehension's map separator, and the caller's loop breaks.
    ///
    /// The tail classification is a parameter because the two grammars
    /// need different tables, deliberately: the label side keeps the
    /// narrow dead-end set (`chain_tail_ends_where` — label disjunctions
    /// commit on operators, `x:A | B AND y` reading as `(x:A|B) AND y`,
    /// a pre-existing behavior), while the value-type side classifies by
    /// the type grammar's continuation whitelist
    /// (`type_pipe_tail_ends_where` — a map expression beginning with a
    /// type-named base, `date(n[k])`, `string[0]`, `string IS NOT NULL`,
    /// must be reached).
    ///
    /// Outside a comprehension-WHERE context the strict grammar is
    /// preserved exactly: the context gate never fires, so a failed
    /// continuation propagates its error — a `|` after a label chain or
    /// type term still demands a continuation, today's disjunction and
    /// union behavior.
    ///
    /// The continuation parses once and is kept on success — nothing
    /// downstream re-parses it, so no verdict memo is involved (unlike
    /// the probes in `speculation.rs`). The revertible region owns
    /// nothing outside its snapshot: whatever `parse` computes beyond
    /// the continuation itself must be returned as part of it (as
    /// `parse_label_or` returns its legacy-`|:` fact), so a rollback
    /// discards that output with the tokens. One deliberate exception:
    /// per-delimiter bookkeeping a caller fires *before* entering the
    /// mechanism (the label loop's `note_gql_label_symbol` at the `|`
    /// itself) sits outside the snapshot and survives a rollback — see
    /// that note's comment in `parse_label_or` and the test pinning it
    /// (`a_rolled_back_pipe_keeps_its_gql_symbol_note`).
    pub(super) fn ambiguous_pipe_continuation<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> ParseResult<T>,
        tail_ends_where: impl Fn(&Self, TokenType) -> bool,
    ) -> ParseResult<Option<T>> {
        // The entry precondition, enforced the way the sibling lookahead
        // gates enforce theirs: the cursor is on the `|` this consumes.
        debug_assert_eq!(self.peek_type(), TokenType::Pipe);
        let pipe_saved_pos = self.save_pos();
        let pipe_saved_flags = self.side_effects;
        self.next_token(); // consume the `|`
        let continuation = parse(self);
        let not_a_continuation = match &continuation {
            Ok(_) => tail_ends_where(self, self.peek_type()),
            Err(_) => true,
        };
        if self.comprehension_where.is_some() && not_a_continuation {
            self.restore_pos(pipe_saved_pos);
            self.side_effects = pipe_saved_flags;
            return Ok(None);
        }
        Ok(Some(continuation?))
    }
}
