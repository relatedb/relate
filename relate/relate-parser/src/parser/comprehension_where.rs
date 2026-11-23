//! The comprehension-WHERE `|` disambiguation — one Parser state family:
//! the context ([`ComprehensionWhereContext`], the value of the
//! `comprehension_where` field on the parser core: the construct's closer
//! token plus the index of its own opener), the one-pass pipe scan
//! ([`PipeScan`], the `comprehension_pipe_scan` field, computed when the
//! context is entered so the per-pipe classification is a lookup, never a
//! re-walk), the one scoped lifecycle owner
//! ([`Parser::with_comprehension_where`]), the one commit-on-success
//! mechanism both `|`-separated grammars resolve the ambiguity through
//! ([`Parser::ambiguous_pipe_continuation`], with the grammar-specific
//! tail classifications living beside their grammars), and the one shared
//! context fact both classifications read
//! ([`Parser::comprehension_where_yields_the_pipe`]) — split out of
//! `mod.rs` the way `speculation.rs` and `side_effects.rs` hold their own
//! state families, so the parser core's field list stays grammar-only.

use super::Parser;
use crate::error::ParseResult;
use crate::parser::speculation::ProbeGate;
use crate::parser::BracketSet;
use crate::token::TokenType;

/// What the `comprehension_where` field carries: the construct's own
/// closer token and the token index of its own opener — the two facts
/// the pipe classification needs. The closer names the form: `]` for the
/// list and pattern comprehension forms (the list form's map expression
/// is optional, the pattern form's required — a distinction no
/// classification reads anymore: both yield a top-level single pipe to
/// the map, live's reading), `}` for the CIP-272 map form. The opener
/// bounds the one-pass [`PipeScan`]'s span, and tells it which brackets
/// are the construct's own and which an inner group's.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ComprehensionWhereContext {
    /// The token that closes the comprehension.
    pub(super) closer: TokenType,
    /// The token index of the comprehension's own opener.
    pub(super) opener: usize,
}

/// The one-pass scan of a comprehension's token span, computed when the
/// comprehension-WHERE context is entered
/// ([`Parser::with_comprehension_where`]) so the per-pipe classification
/// is a pair of lookups — never a re-walk per pipe, which a long chain
/// (`x:A|B|…|Z | x`) would turn quadratic. Records the bracket depth
/// after every token of the span, the last top-level `|` before the
/// closer (the only one any classification asks about), and the
/// closer's own position.
pub(super) struct PipeScan {
    /// The token index of the comprehension's own opener — the span's
    /// first token is one past it.
    opener: usize,
    /// The bracket depth (relative to the construct's interior) after
    /// each token from `opener + 1` on: `depths[k]` is the depth after
    /// token `opener + 1 + k`, so the depth in effect just before
    /// position `pos` is `depths[pos - opener - 1]`.
    depths: Vec<u32>,
    /// The position of the last top-level `|` before the closer — the
    /// one fact the yield question reads ("is there a further pipe at or
    /// after the cursor"): the scan walks forward, so the last pipe it
    /// saw is the last one there is.
    last_top_level_pipe: Option<usize>,
    /// The position of the construct's own closer, when the span closes
    /// within the walk cap below.
    closer: Option<usize>,
}

/// The walk cap: the longest span the one-pass scan materialises. A span
/// past it is degenerate or adversarial (a real comprehension-WHERE
/// chain is a few tokens per arm); the scan stops and reports no closer,
/// which makes every classification keep the strict chain grammar — the
/// pre-branch behavior — instead of yielding. The cap bounds the depths
/// vector (`MAX_PIPE_SCAN_TOKENS` u32 entries per active nesting level,
/// so the full `MAX_NESTING_DEPTH` stack stays in single-digit
/// megabytes) where an uncapped span times 500 nesting levels could
/// otherwise allocate unbounded, uncharged parse memory from one
/// statement.
const MAX_PIPE_SCAN_TOKENS: usize = 4096;

/// How many separator confirmations may nest inside each other before a
/// further one keeps the chain grammar instead — see
/// [`Parser::separator_reading_parses`]. Above it the worst-case
/// confirmation work is capped at this depth squared times the
/// per-level span; the memoized-confirmations complexity pin parses 25
/// nested levels, comfortably under it.
const MAX_CONFIRMATION_DEPTH: u32 = 32;

/// The bracket vocabulary the scan counts — every bracket kind nests
/// inside a comprehension's span (a source list literal, a call's
/// argument parens, a map literal's braces).
const BRACKET_SET: BracketSet = BracketSet::AllBrackets;

impl PipeScan {
    /// The one forward walk: from the token after the opener to the
    /// construct's own closer (the context's token — a depth-0 closer of
    /// any other kind is an inner group's, transparent), the walk cap,
    /// or EOF — depth relative to the interior.
    fn of(parser: &Parser<'_>, context: ComprehensionWhereContext) -> Self {
        let mut depths = Vec::new();
        let mut last_top_level_pipe = None;
        let mut closer = None;
        let mut depth = 0u32;
        let mut pos = context.opener + 1;
        let span_end = context.opener + 1 + MAX_PIPE_SCAN_TOKENS;
        loop {
            let ty = parser.token_type_at(pos);
            if ty == TokenType::EOF || pos >= span_end {
                break;
            }
            match ty {
                _ if BRACKET_SET.opens(ty) => depth += 1,
                _ if BRACKET_SET.closes(ty) => {
                    if depth == 0 && ty == context.closer {
                        // The construct's own closer ends the span.
                        closer = Some(pos);
                        depths.push(0);
                        break;
                    }
                    // A depth-0 closer of another kind, or an inner
                    // group's closer: the inner grammar's, transparent.
                    depth = depth.saturating_sub(1);
                }
                TokenType::Pipe if depth == 0 => last_top_level_pipe = Some(pos),
                _ => {}
            }
            depths.push(depth);
            pos += 1;
        }
        Self {
            opener: context.opener,
            depths,
            last_top_level_pipe,
            closer,
        }
    }

    /// The depth in effect just before `pos` — the depth after the
    /// token before it. `None` when `pos` sits outside the scanned span
    /// (at or before the opener, or past a closer the scan stopped at).
    fn depth_before(&self, pos: usize) -> Option<u32> {
        // How many span tokens sit strictly before `pos` (the span's
        // first token is one past the opener).
        let before = pos.checked_sub(self.opener + 1)?;
        if before == 0 {
            // At the span's first token: the construct's own top level.
            return Some(0);
        }
        // The depth after the (`before`-th) span token — the token just
        // before `pos`.
        self.depths.get(before - 1).copied()
    }

    /// Whether a top-level `|` sits at or after `pos` in the span — the
    /// last pipe the forward walk saw is the last one there is, so one
    /// comparison answers.
    fn has_top_level_pipe_at_or_after(&self, pos: usize) -> bool {
        self.last_top_level_pipe.is_some_and(|pipe| pipe >= pos)
    }
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
        context: Option<ComprehensionWhereContext>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let saved = self.comprehension_where;
        let saved_scan = self.comprehension_pipe_scan.take();
        self.comprehension_where = context;
        self.comprehension_pipe_scan = context.map(|c| PipeScan::of(self, c));
        let result = f(self);
        self.comprehension_where = saved;
        self.comprehension_pipe_scan = saved_scan;
        result
    }

    /// The one shared context fact both pipe classifiers read
    /// ([`Parser::chain_tail_ends_where`] on the label side,
    /// [`Parser::type_pipe_tail_ends_where`] on the value-type side): at
    /// pattern-element depth zero, the comprehension yields its pipe to
    /// the map separator — live Neo4j's reading, which prefers the
    /// separator whenever the rest parses as the map expression and
    /// keeps the disjunction/union alive only when a *later* pipe in the
    /// same comprehension makes the separator reading unparseable.
    /// Inside a pattern element's own label/type chain the separator
    /// question never arises, so the fact is gated off there and each
    /// grammar's own continuation rule decides.
    pub(super) fn comprehension_where_yields_the_pipe(&self) -> bool {
        self.pattern_element_depth == 0 && self.closer_precedes_any_top_level_pipe()
    }

    /// The scan lookup behind [`Self::comprehension_where_yields_the_pipe`]:
    /// true when the construct's own closer arrives at top level before
    /// any further `|`. Two lookups into the context's one-pass
    /// [`PipeScan`] — never a re-walk:
    ///
    /// - *Nesting first.* A pipe whose cursor sits inside an open inner
    ///   group (a bracket opened after the comprehension's own opener and
    ///   not yet closed) is the inner grammar's, never the
    ///   comprehension's separator: the depth just before the cursor
    ///   settles it (the map-comprehension source's `[n:A|B]`, a union
    ///   inside call arguments, a list literal in the WHERE).
    /// - *Then the closer.* At the comprehension's top level, the pipe
    ///   yields to the map separator exactly when no top-level `|` sits
    ///   at or after the cursor — a further pipe keeps the
    ///   disjunction/union alive, the closer (or the span's end:
    ///   malformed, unterminated input keeps the continuation reading
    ///   either way) yields it.
    fn closer_precedes_any_top_level_pipe(&self) -> bool {
        let Some(scan) = &self.comprehension_pipe_scan else {
            return false;
        };
        match scan.depth_before(self.pos) {
            Some(depth) if depth > 0 => false,
            // At top level the pipe yields exactly when the construct's
            // own closer arrives before any further top-level `|`.
            Some(_) => scan.closer.is_some() && !scan.has_top_level_pipe_at_or_after(self.pos),
            // Outside the span, or a capped/unclosed one: the fact
            // answers no — the grammars' own classifiers may add their
            // unscanned-span fallbacks (see
            // [`Self::comprehension_scan_unanswered`]).
            None => false,
        }
    }

    /// Whether the active comprehension-WHERE context's span could not be
    /// answered by the one-pass scan — past the walk cap, or never
    /// closed. The label side's own classifier keeps its pre-branch
    /// map-base postfix fallback (`.`/`(`) for exactly these spans; the
    /// value-type side's continuation whitelist yields on those tails
    /// without help.
    pub(super) fn comprehension_scan_unanswered(&self) -> bool {
        self.comprehension_pipe_scan
            .as_ref()
            .is_some_and(|scan| scan.closer.is_none())
    }

    /// The confirmation's closer check: the just-parsed map expression
    /// ends at the comprehension's own closer — the scan's position when
    /// the span closed, or (a capped or unclosed span) the context's own
    /// closer token as the very next token.
    fn map_ends_at_the_closer(&self) -> bool {
        let Some(scan) = &self.comprehension_pipe_scan else {
            return false;
        };
        if let Some(closer) = scan.closer {
            return closer == self.pos;
        }
        self.comprehension_where
            .is_some_and(|c| c.closer == self.peek_type())
    }

    /// The confirmation behind the comprehension-WHERE yield
    /// ([`Self::ambiguous_pipe_continuation`]): whether the tokens after
    /// the pipe parse as the comprehension's map expression — the
    /// grammar-owned `pipe_prefix` (the label side's legacy `|:`
    /// spelling), the map expression itself, then the comprehension's
    /// own closer (the scan's position, or the context's closer token
    /// for a capped span) as the very next token. Routed through the one
    /// memoized always-restore policy ([`Self::probe_verdict`]): the
    /// position-and-flags snapshot is that policy's own, and the verdict
    /// memo keys on the probe's position, so nested comprehension WHEREs
    /// — whose map-expression parses contain their own ambiguous pipes
    /// and confirmations — parse each position once, never
    /// exponentially (the security round's shape).
    fn separator_reading_parses(
        &mut self,
        pipe_pos: usize,
        pipe_prefix: Option<TokenType>,
    ) -> bool {
        // The confirmation depth cap: a confirmation whose parse is itself
        // nested inside more than a few enclosing confirmations keeps the
        // chain grammar (the merge-base behavior) instead — each
        // enclosing level's memoized confirmation re-walks its tail once,
        // so unbounded depth-on-depth would still amplify parse work
        // linearly per level on adversarial input; the cap bounds the
        // nesting of the walks themselves. Real queries never reach it
        // (a handful of levels at most); the memoized-confirmations
        // complexity pin parses 25 levels well under it.
        if self.speculation.depth >= MAX_CONFIRMATION_DEPTH {
            return false;
        }
        self.probe_verdict(ProbeGate::SeparatorConfirmation, |parser| {
            parser.restore_pos(pipe_pos);
            // consume the `|` again, then the grammar-owned pipe
            // prefix if the caller's grammar spells one (the label
            // side's legacy `|:`).
            parser.next_token();
            if pipe_prefix.is_some_and(|prefix| parser.peek_type() == prefix) {
                parser.next_token();
            }
            parser.parse_expression().is_ok() && parser.map_ends_at_the_closer()
        })
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
    /// resolve the pipe differently, deliberately: the label side prefers
    /// the map separator whenever the rest parses as the map expression
    /// (`chain_tail_ends_where` — one fact,
    /// [`Self::closer_precedes_any_top_level_pipe`]: the disjunction
    /// survives only when a later pipe in the same comprehension makes
    /// the separator reading unparseable, live Neo4j's rule), while the
    /// value-type side classifies by the type grammar's continuation
    /// whitelist (`type_pipe_tail_ends_where` — a map expression
    /// beginning with a type-named base, `date(n[k])`, `string[0]`,
    /// `string IS NOT NULL`, must be reached).
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
        tail_ends_where: impl Fn(&Self) -> bool,
        pipe_prefix: Option<TokenType>,
    ) -> ParseResult<Option<T>> {
        // The entry precondition, enforced the way the sibling lookahead
        // gates enforce theirs: the cursor is on the `|` this consumes.
        debug_assert_eq!(self.peek_type(), TokenType::Pipe);
        let pipe_saved_pos = self.save_pos();
        let pipe_saved_flags = self.side_effects;
        self.next_token(); // consume the `|`
        let continuation = parse(self);
        let not_a_continuation = match &continuation {
            Ok(_) => tail_ends_where(self),
            Err(_) => true,
        };
        // Live's rule is a parse preference: the separator reading wins
        // only when the rest actually parses as the map expression. The
        // closer-first fact is necessary but not sufficient — a
        // continuation that cannot start a map expression (`!B`,
        // `$(x.p)`, a bracketed type `LIST<INTEGER>`) or a tail that only
        // continues the chain grammar (`x:A|B&C`) leaves the separator
        // reading unparseable, and the chain reading must survive (each
        // of those parsed before this branch). One speculative
        // expression parse from the pipe settles it, memoized through
        // the shared probe-verdict policy so nested comprehension WHEREs
        // never re-parse exponentially.
        let yields = self.comprehension_where.is_some()
            && not_a_continuation
            && self.separator_reading_parses(pipe_saved_pos, pipe_prefix);
        if yields {
            self.restore_pos(pipe_saved_pos);
            self.side_effects = pipe_saved_flags;
            return Ok(None);
        }
        Ok(Some(continuation?))
    }
}
