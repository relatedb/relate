//! Recognition of the administration commands Neo4j CE rejects (ADR-0116
//! §2, LABS-873): the one table-driven recognizer behind every
//! `UnsupportedAdministrationCommand` (51N27) guard in the clause parsers.
//!
//! None of the command words is reserved, so each guard looks a few tokens
//! ahead. A command form is a [`AdminShape`] -- the slots its leading
//! tokens fill -- and each dispatch site owns one table of shapes; adding a
//! command is one row, never a new hand-spelled arm.

use super::*;

/// One token position of a command shape.
enum Slot {
    /// One identifier spelling (any case).
    Word(&'static str),
    /// Any one of several identifier spellings.
    AnyWord(&'static [&'static str]),
    /// A name: any identifier-like token.
    Name,
    /// The reserved `FOR` keyword.
    For,
}

/// The leading tokens of one unsupported command form.
pub(super) struct AdminShape {
    slots: &'static [Slot],
    /// A variable may share the shape's spelling (`CREATE user = (a)-[]->(b)`
    /// names a path): when set, the token after the shape must not be `=`.
    not_path_assignment: bool,
}

const USER_OR_ROLE: Slot = Slot::AnyWord(&["user", "role"]);

/// The database-family targets of `CREATE` and `DROP`; `CREATE` guards the
/// bare-word ones against a same-named path variable.
const fn database_family(guarded: bool) -> [AdminShape; 4] {
    [
        AdminShape {
            slots: &[Slot::Word("database")],
            not_path_assignment: guarded,
        },
        AdminShape {
            slots: &[Slot::Word("composite"), Slot::Word("database")],
            not_path_assignment: guarded,
        },
        AdminShape {
            slots: &[Slot::Word("alias"), Slot::Name, Slot::For],
            not_path_assignment: false,
        },
        AdminShape {
            slots: &[USER_OR_ROLE],
            not_path_assignment: guarded,
        },
    ]
}

/// Commands opening a statement with an identifier. `GRANT`/`DENY`/`REVOKE`
/// take no lookahead: their argument is a privilege, a vocabulary this
/// parser has no other use for, and no clause starts with a bare
/// identifier, so reaching the guard already means the statement opens with
/// the command. `RENAME`/`START`/`STOP` look ahead because each is a
/// plausible variable name.
pub(super) const CLAUSE_START_SHAPES: &[AdminShape] = &[
    AdminShape {
        slots: &[Slot::Word("start"), Slot::Word("database")],
        not_path_assignment: false,
    },
    AdminShape {
        slots: &[Slot::Word("stop"), Slot::Word("database")],
        not_path_assignment: false,
    },
    AdminShape {
        slots: &[Slot::AnyWord(&["grant", "deny", "revoke"])],
        not_path_assignment: false,
    },
    AdminShape {
        slots: &[Slot::Word("rename"), USER_OR_ROLE],
        not_path_assignment: false,
    },
    // `ALTER DATABASE` and `ALTER CURRENT GRAPH TYPE` are real clauses and
    // fall through to the contextual dispatch.
    AdminShape {
        slots: &[
            Slot::Word("alter"),
            Slot::AnyWord(&["alias", "user", "role"]),
        ],
        not_path_assignment: false,
    },
];

/// The targets after `CREATE [OR REPLACE]`.
pub(super) const CREATE_SHAPES: &[AdminShape] = &database_family(true);

/// The targets after `DROP`.
pub(super) const DROP_SHAPES: &[AdminShape] = &database_family(false);

impl<'a> Parser<'a> {
    fn slot_matches(&self, slot: &Slot, at: usize) -> bool {
        let tok = self.peek_at(at);
        match slot {
            Slot::Word(word) => Self::token_is_ident(tok, word),
            Slot::AnyWord(words) => words.iter().any(|w| Self::token_is_ident(tok, w)),
            Slot::Name => tok.ty.is_ident_like(),
            Slot::For => tok.ty == TokenType::FOR,
        }
    }

    /// True when the tokens from `offset` ahead fill one of `shapes`.
    pub(super) fn at_admin_command(&self, shapes: &[AdminShape], offset: usize) -> bool {
        shapes.iter().any(|shape| {
            shape
                .slots
                .iter()
                .enumerate()
                .all(|(i, slot)| self.slot_matches(slot, offset + i))
                && !(shape.not_path_assignment
                    && self.peek_at(offset + shape.slots.len()).ty == TokenType::Equals)
        })
    }

    /// The `CREATE [OR REPLACE] <target>` guard: rejects an unsupported
    /// administration target, leaving a real `CREATE` untouched. `start` is
    /// the position of the `CREATE` token; it has been consumed.
    pub(super) fn reject_unsupported_admin_create(&mut self, start: usize) -> ParseResult<()> {
        let or_replace = self.peek_type() == TokenType::OR && self.peek_at_is_ident(1, "replace");
        if self.at_admin_command(CREATE_SHAPES, if or_replace { 2 } else { 0 }) {
            return Err(self.unsupported_admin_error(start));
        }
        Ok(())
    }

    /// The heads `DROP` accepts as unsupported administration targets, as
    /// the uppercase spellings the expected-target diagnostic names --
    /// derived from [`DROP_SHAPES`] so the two cannot drift.
    pub(super) fn drop_admin_target_words() -> Vec<String> {
        DROP_SHAPES
            .iter()
            .flat_map(|shape| match shape.slots.first() {
                Some(Slot::Word(word)) => vec![word.to_ascii_uppercase()],
                Some(Slot::AnyWord(words)) => {
                    words.iter().map(|w| w.to_ascii_uppercase()).collect()
                }
                _ => Vec::new(),
            })
            .collect()
    }

    /// ADR-0116 §2: the parser recognizes every database administration
    /// command Neo4j CE rejects, so the engine answers with CE's
    /// `UnsupportedAdministrationCommand` (51N27) rather than a syntax
    /// error. Consumes the rest of the statement and returns the
    /// [`ParseError`] tagged [`UNSUPPORTED_ADMINISTRATION_DETAIL`] whose
    /// message is the verbatim statement text (sliced from `start_pos`, the
    /// command's first token, so any leading `CYPHER n` preamble is
    /// excluded) -- fail-closed: an unsupported administration command
    /// never produces an executable clause. `relate-front` maps this
    /// parse error to CE's semantic error text.
    pub(in crate::parser) fn unsupported_admin_error(&mut self, start_pos: usize) -> ParseError {
        while !matches!(self.peek_type(), TokenType::EOF | TokenType::Semicolon) {
            self.next_token();
        }
        // Internal invariant, not user input: `start_pos` is the command's
        // first token, saved by the caller before any of the command was
        // consumed, so it always indexes a real token and the source slice
        // from it to the current position always exists. A violation is a
        // parser bug -- assert loudly instead of masking it with a fallback.
        let text = self.extract_source_text(start_pos, self.pos).expect(
            "invariant: unsupported_admin_error start_pos indexes the command's first token, \
             so its source slice always exists",
        );
        let tok = &self.tokens[start_pos];
        ParseError::new(text, tok.line, tok.col)
            .with_source(self.source)
            .with_detail(UNSUPPORTED_ADMINISTRATION_DETAIL)
    }

    /// [`Self::unsupported_admin_error`] as a clause-position result, for
    /// the sites whose own return type is a `Clause`.
    pub(in crate::parser) fn unsupported_admin_clause(
        &mut self,
        start_pos: usize,
    ) -> ParseResult<Clause> {
        Err(self.unsupported_admin_error(start_pos))
    }
}
