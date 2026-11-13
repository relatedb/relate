use super::*;

use crate::token::token_start_byte;

/// The three hint kinds `parse_using_hints` keeps: the `USING SCAN`
/// variables (the executor honours them), the `USING INDEX` hints (the
/// front end validates their fulfillability), and the `USING JOIN ON`
/// hints (the planner never fulfils one, so the `01N30` notification
/// reports each).
#[derive(Debug, Default)]
pub(super) struct UsingHints {
    pub(super) scan: Vec<crate::ast::ScanHint>,
    pub(super) index: Vec<crate::ast::IndexHint>,
    pub(super) join: Vec<crate::ast::JoinHint>,
}

impl<'a> Parser<'a> {
    /// Label-expression mixing (`InvalidLabelExpression`/
    /// `MixedIsAndColonConjunction` for pattern-position label exprs) is
    /// scoped to one *top-level clause* (CIP-40 Section 3.6) -- but a
    /// clause containing an `EXISTS {}`/`COUNT {}` subquery expression is
    /// one clause together with everything nested inside that subquery's
    /// own statement body: `statement_depth == 1` exactly while parsing a
    /// clause of the *outermost* statement, so only that outer call
    /// resets/checks the flags; a `parse_clause` call nested inside a
    /// subquery's own `parse_statement` (`statement_depth >= 2`) leaves them
    /// alone, letting the subquery's label usage bubble up and combine with
    /// the enclosing clause's own (label-expression-mixing.feature scenario
    /// 12: two MATCH clauses nested inside one `COUNT {}` combine and must
    /// error) -- while two genuinely separate top-level clauses do NOT
    /// combine (label-expression-mixing.feature scenario 19,
    /// orthogonal-features-and-match-modes.feature scenario 9). A `CALL {}`
    /// subquery's body is one scope of its own, opened and closed by
    /// `parse_call_subquery_statement` (Neo4j GpmSyntaxMixingAcceptance
    /// "Conflicting syntax in separate statements within a CALL subquery").
    /// The `mixed-label-syntax` preview opt-in suspends the label-mixing
    /// rejections. Quantifier
    /// mixing, unlike label mixing, is scoped strictly per literal clause
    /// regardless of nesting (quantifier-mixing.feature scenario 17's
    /// COUNT-subquery exception), so it always resets here.
    pub(super) fn parse_clause(&mut self) -> ParseResult<Clause> {
        // ADR-0257: nested FOREACH bodies recurse `parse_clause` ->
        // `parse_clause_inner` -> `parse_foreach` -> `parse_clause`,
        // bypassing `parse_statement`, so the `statement_depth` cap never
        // engages on that cycle; count clause nesting here instead
        // (LABS-69). Sequential clauses in a statement or FOREACH body do
        // not accumulate: each `parse_clause` call fully returns before the
        // enclosing loop makes the next one, so the counter only grows
        // with true nesting.
        self.with_clause_depth(Self::parse_clause_at_current_depth)
    }

    pub(super) fn parse_clause_at_current_depth(&mut self) -> ParseResult<Clause> {
        // A clause inside a `CALL {}` subquery body accumulates into the
        // body's own scope (see `parse_call_subquery_statement`), exactly
        // like a clause inside an `EXISTS {}`/`COUNT {}` subquery
        // expression accumulates into the enclosing clause's: only a
        // top-level clause opens and closes a scope here.
        let is_outer_clause = self.statement_depth <= 1;
        if is_outer_clause {
            self.side_effects = PatternSideEffectFlags::default();
        } else {
            // Label-mixing state bubbles up out of subquery bodies (see the
            // `parse_clause` doc comment); only the clause-local flags and
            // the quantifier-mixing flags reset per literal clause
            // regardless of nesting.
            self.side_effects.clause_labels = LabelMixFlags::default();
            self.side_effects.quantifiers = QuantifierForms::default();
            self.side_effects.comprehension_quantifiers = QuantifierForms::default();
        }
        // The clause decides which label-mixing accumulator its forms feed;
        // a nested body's clauses set their own and restore this one.
        let target = self.label_mix_target_for_current_clause();
        let saved_target = std::mem::replace(&mut self.label_mix_target, target);
        let clause_result = self.parse_clause_inner();
        self.label_mix_target = saved_target;
        let clause = clause_result?;
        // The `mixed-label-syntax` preview opt-in (ADR-0371, threaded from
        // the store's `RELATE_PREVIEW` set) lifts the CIP-40/CIP-14
        // label-mixing rejections: Neo4j's
        // mixed-label-syntax semantic feature. The floor
        // (opt-in off) keeps every rejection below.
        if is_outer_clause {
            self.check_label_mixing(&self.side_effects)?;
        }
        // Quantifier mixing never lifts: the clause's own forms combine,
        // and the clause's comprehensions' forms combine with each other,
        // but a comprehension's forms never combine with the clause's own
        // (see `comprehension_quantifiers`).
        self.check_quantifier_mixing()?;
        Ok(clause)
    }

    /// The clause-level quantifier-mixing rejection, shared in shape with
    /// [`Self::check_label_mixing`].
    fn check_quantifier_mixing(&self) -> ParseResult<()> {
        let flags = &self.side_effects;
        if [flags.quantifiers, flags.comprehension_quantifiers]
            .iter()
            .any(|forms| forms.mixed())
        {
            return Err(self.error(
                "MixedQuantifiers: cannot mix the legacy bracket-syntax variable-length quantifier with a GPM postfix/QPP quantifier within the same clause",
            ));
        }
        Ok(())
    }

    pub(super) fn parse_clause_inner(&mut self) -> ParseResult<Clause> {
        let tok = self.peek().clone();
        match tok.ty {
            TokenType::MATCH => self.parse_match(false),
            // CIP-159: `OPTIONAL MATCH` (pre-existing) and `OPTIONAL CALL`
            // (inline subquery or named procedure call) -- `OPTIONAL` must
            // immediately precede one of these two clause keywords.
            TokenType::OPTIONAL => {
                self.next_token(); // consume OPTIONAL
                match self.peek_type() {
                    TokenType::MATCH => self.parse_match(true),
                    TokenType::CALL => self.parse_call(true),
                    _ => Err(self.error(format!(
                        "expected MATCH or CALL, got {:?}",
                        self.peek().literal
                    ))),
                }
            }
            TokenType::CREATE => self.parse_create(),
            TokenType::INSERT => self.parse_insert(),
            TokenType::MERGE => self.parse_merge(),
            TokenType::DELETE => self.parse_delete(false),
            TokenType::DETACH => {
                self.next_token(); // consume DETACH
                self.expect(TokenType::DELETE)?;
                self.backup();
                self.parse_delete(true)
            }
            // CIP-122: `NODETACH DELETE <delete item list>` -- an explicit
            // spelling of the (already-default) plain, non-detaching DELETE.
            // Not a reserved keyword (recognized by spelling, like
            // `SHOW`/`USE`/`LET`/`FILTER` above) so it stays usable as an
            // ordinary variable name everywhere except directly before
            // DELETE at clause start.
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("nodetach") => {
                self.next_token(); // consume NODETACH
                self.expect(TokenType::DELETE)?;
                self.backup();
                self.parse_delete(false)
            }
            TokenType::SET => self.parse_set(),
            TokenType::REMOVE => self.parse_remove(),
            TokenType::RETURN => self.parse_return(),
            TokenType::WITH => self.parse_with(),
            TokenType::UNWIND => self.parse_unwind(),
            // CIP-250 (GQ10): `FOR <variable> IN <expression>`, the
            // GQL-compliant spelling of UNWIND -- valid wherever UNWIND is
            // valid. `FOREACH` is its own token, so the two never collide.
            TokenType::FOR => self.parse_for_clause(),
            TokenType::FOREACH => self.parse_foreach(),
            TokenType::CALL => self.parse_call(false),
            TokenType::LOAD => self.parse_load_csv(),
            TokenType::DROP => self.parse_drop(),
            // The contextual (identifier-spelled) clause keywords --
            // single-sourced as `CONTEXTUAL_CLAUSE_WORDS` in token.rs so
            // `peek_starts_show_filter_expr` excludes exactly this set.
            // None of them is a reserved keyword (each is only meaningful
            // at clause start, e.g. `SHOW FUNCTIONS`); recognized by
            // spelling on a plain Identifier token, same technique as the
            // GQL special forms (trim/normalize/property_exists) further
            // down.
            TokenType::Identifier if is_contextual_clause_keyword(&tok.literal) => {
                self.parse_contextual_clause(&tok)
            }
            // ADR-0116 §2: `START DATABASE <name>` / `STOP DATABASE
            // <name>` -- recognized, then rejected with CE's
            // `UnsupportedAdministrationCommand`. `START`/`STOP` stay
            // ordinary identifiers everywhere else (`MATCH (start)
            // WHERE start.alias = 'stop' ...` keeps parsing).
            TokenType::Identifier
                if (tok.literal.eq_ignore_ascii_case("start")
                    || tok.literal.eq_ignore_ascii_case("stop"))
                    && self.peek_at_is_ident(1, "database") =>
            {
                let start = self.pos;
                self.unsupported_admin_clause(start)
            }
            // CIP-168: standalone `<order by and page statement>` -- a new
            // clause starting directly with ORDER BY / SKIP / OFFSET /
            // LIMIT rather than attached to WITH/RETURN/YIELD.
            TokenType::ORDER | TokenType::SKIP | TokenType::LIMIT => {
                self.parse_standalone_order_by_page()
            }
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("offset") => {
                self.parse_standalone_order_by_page()
            }
            _ => Err(self.error_at_token(&tok, format!("unexpected token {:?}", tok.literal))),
        }
    }

    // ---- Clause parsers ----

    /// Dispatch for the contextual (identifier-spelled) clause keywords
    /// ([`crate::token::CONTEXTUAL_CLAUSE_WORDS`]), reached from
    /// `parse_clause_inner`'s single consolidated guard. `NEXT`/`CYPHER`
    /// are in the set but never start a clause here (statement-block
    /// boundary / pre-parser preamble respectively), so they get the same
    /// `unexpected token` error any unrecognized clause start gets.
    fn parse_contextual_clause(&mut self, tok: &Token) -> ParseResult<Clause> {
        let word = tok.literal.to_ascii_lowercase();
        match word.as_str() {
            "show" => self.parse_show_dispatch(),
            // CIP-68: `TERMINATE TRANSACTION[S] <ids> [YIELD ...]`.
            "terminate" => self.parse_terminate_transactions(),
            // CIP-141: `USE <graph-expr>` graph-selector clause.
            "use" => self.parse_use().map(Clause::Use),
            // CIP-206: `LET <var> = <expr>, ...` mid-query projection.
            "let" => self.parse_let(),
            // CIP-203: `FILTER [WHERE] <search condition>`.
            "filter" => self.parse_filter(),
            // CIP-140: `FINISH` terminal clause.
            "finish" => self.parse_finish(),
            "alter" => {
                // CIP-170: `ALTER DATABASE ...` is distinguished by the word
                // directly after `ALTER` (`DATABASE` vs `CURRENT`).
                if self.peek_at_is_ident(1, "database") {
                    self.parse_alter_database()
                } else if self.peek_at_is_ident(1, "alias") {
                    // ADR-0116 §2: `ALTER ALIAS <name> SET DATABASE <target>`
                    // -- recognized, then rejected with CE's
                    // `UnsupportedAdministrationCommand`.
                    let start = self.pos;
                    self.unsupported_admin_clause(start)
                } else {
                    // CIP-182/CIP-187: `ALTER CURRENT GRAPH TYPE SET { ... }`.
                    self.parse_alter_current_graph_type()
                }
            }
            _ => Err(self.error_at_token(tok, format!("unexpected token {:?}", tok.literal))),
        }
    }

    pub(super) fn parse_create(&mut self) -> ParseResult<Clause> {
        let start = self.pos;
        self.next_token(); // consume CREATE

        // CIP-170: `CREATE [OR REPLACE] DATABASE ...` / `CREATE [OR
        // REPLACE] COMPOSITE DATABASE ...`. Checked before the general
        // dispatch below -- a plain `CREATE (...)` node pattern can never
        // start with `OR`. `database`/`composite` aren't reserved words
        // though, so they *can* legitimately be a named-path variable
        // (`CREATE database = (a)-[:X]->(b)`, CIP-146's `<named path>`
        // form) -- disambiguated by checking the token immediately after
        // isn't `=`, which a `<create database>`'s `<name>` position can
        // never be followed by.
        //
        // ADR-0116 §2: with fixed namespaces, every `CREATE [OR REPLACE]
        // [COMPOSITE] DATABASE` form is recognized and then rejected with
        // CE's `UnsupportedAdministrationCommand` rather than executed.
        {
            let or_replace =
                self.peek_type() == TokenType::OR && self.peek_at_is_ident(1, "replace");
            let after = if or_replace { 2 } else { 0 };
            if self.peek_at_is_ident(after, "database")
                && self.peek_at(after + 1).ty != TokenType::Equals
            {
                return self.unsupported_admin_clause(start);
            }
            if self.peek_at_is_ident(after, "composite")
                && self.peek_at_is_ident(after + 1, "database")
                && self.peek_at(after + 2).ty != TokenType::Equals
            {
                return self.unsupported_admin_clause(start);
            }
            // ADR-0116 §2: `CREATE [OR REPLACE] ALIAS <name> FOR
            // DATABASE <target>`. Same named-path disambiguation as
            // above: an `ALIAS` variable is never followed by `<name>
            // FOR`.
            if self.peek_at_is_ident(after, "alias")
                && self.peek_at(after + 1).ty.is_ident_like()
                && self.peek_at(after + 2).ty == TokenType::FOR
            {
                return self.unsupported_admin_clause(start);
            }
        }

        match self.peek_type() {
            TokenType::INDEX => {
                return Ok(Clause::CreateIndex(
                    self.parse_create_index(IndexKind::Range)?,
                ));
            }
            TokenType::VECTOR => {
                self.next_token();
                self.expect(TokenType::INDEX)?;
                self.backup();
                return Ok(Clause::CreateIndex(
                    self.parse_create_index(IndexKind::Vector)?,
                ));
            }
            TokenType::POINT => {
                self.next_token();
                self.expect(TokenType::INDEX)?;
                self.backup();
                return Ok(Clause::CreateIndex(
                    self.parse_create_index(IndexKind::Point)?,
                ));
            }
            TokenType::CONSTRAINT => {
                return Ok(Clause::CreateConstraint(self.parse_create_constraint()?));
            }
            // CIP-178/CIP-180: `CREATE GRAPH TYPE <name> [AS] { ... }`.
            // `GRAPH`/`TYPE` aren't reserved tokens (like `RANGE`/`TEXT`/...
            // INDEX below), recognized by spelling only directly before
            // `TYPE` -- an ordinary `CREATE (graph:Type)`-style pattern
            // never has an identifier named `graph` immediately followed by
            // one named `type`.
            TokenType::Identifier
                if self.peek().literal.eq_ignore_ascii_case("graph")
                    && self.peek_at(1).ty == TokenType::Identifier
                    && self.peek_at(1).literal.eq_ignore_ascii_case("type") =>
            {
                return self.parse_create_graph_type();
            }
            // RANGE/TEXT/FULLTEXT/LOOKUP/BTREE aren't reserved tokens (they
            // lex as plain identifiers, like `SHOW`/`USE`/`LET` elsewhere in
            // this parser) so they're recognized by spelling, only directly
            // before INDEX -- an ordinary `CREATE p = (...)` named-path
            // pattern never has an identifier immediately followed by
            // INDEX, so there's no ambiguity with a path variable named
            // e.g. `range`.
            TokenType::Identifier if self.peek_at(1).ty == TokenType::INDEX => {
                let word = self.peek().literal.to_ascii_lowercase();
                let kind = match word.as_str() {
                    "range" => IndexKind::Range,
                    "text" => IndexKind::Text,
                    "fulltext" => IndexKind::Fulltext,
                    "lookup" => IndexKind::Lookup,
                    "btree" => {
                        let tok = self.peek().clone();
                        return Err(self.error_at_token(
                            &tok,
                            "UnsupportedIndexType: BTREE indexes were removed; use RANGE, POINT, TEXT, FULLTEXT, LOOKUP, or VECTOR"
                                .to_string(),
                        ));
                    }
                    _ => {
                        let pattern = self.parse_pattern()?;
                        return Ok(Clause::Create(CreateClause {
                            pattern,
                            is_insert: false,
                        }));
                    }
                };
                self.next_token(); // consume the kind word
                self.expect(TokenType::INDEX)?;
                self.backup();
                return Ok(Clause::CreateIndex(self.parse_create_index(kind)?));
            }
            _ => {}
        }

        let pattern = self.parse_pattern()?;
        Ok(Clause::Create(CreateClause {
            pattern,
            is_insert: false,
        }))
    }

    /// The GQL `INSERT` clause (CIP-40 Section 2.3.3 / Section 4.1 DML row):
    /// a legal alternate to `CREATE`, executed identically, but its label
    /// set specifications may not use the legacy colon-conjunction shorthand
    /// (`:A:B`) -- only `IS`-introduced (optionally `&`-composed) or a
    /// single bare `:Label` are allowed.
    pub(super) fn parse_insert(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume INSERT
        let pattern = self.parse_pattern()?;
        if self.side_effects.clause_labels.legacy_conjunction {
            return Err(self.error(
                "42I54: Colon ':' conjunction is not allowed in INSERT. Use CREATE or conjunction with ampersand '&' instead.",
            ));
        }
        // CIP-114: parenthesized label expressions (`(A:B):C`) are a CREATE-
        // only grouping form -- INSERT's grammar doesn't accept `(` in
        // label-expression position at all.
        if self.side_effects.label_saw_paren_group {
            return Err(self.error("42I06: Invalid input '(', expected an identifier."));
        }
        // CIP-114: named path patterns (`INSERT p = (a)-[r]->(b)`) are not
        // allowed in INSERT -- only in CREATE.
        if pattern.iter().any(|p| p.variable.is_some()) {
            return Err(self.error(
                "42I54: Named patterns are not allowed in INSERT. Use CREATE or remove the name.",
            ));
        }
        Ok(Clause::Create(CreateClause {
            pattern,
            is_insert: true,
        }))
    }

    pub(super) fn parse_drop(&mut self) -> ParseResult<Clause> {
        let start = self.pos;
        self.next_token(); // consume DROP

        let tok = self.peek().clone();
        match tok.ty {
            TokenType::INDEX => {
                self.next_token();
                if self.peek_type() == TokenType::ON {
                    let target_tok = self.peek().clone();
                    let (label, properties) = self.parse_legacy_index_target()?;
                    let [property]: [SmallName; 1] = properties.try_into().map_err(|_| {
                        self.error_at_token(
                            &target_tok,
                            "legacy DROP INDEX ON :Label(prop) takes exactly one property",
                        )
                    })?;
                    return Ok(Clause::DropIndex(DropIndexClause {
                        name: SchemaName::Literal(String::new()),
                        if_exists: false,
                        legacy_target: Some((label, property)),
                    }));
                }
                let (name, if_exists) = self.parse_drop_target("DROP INDEX")?;
                Ok(Clause::DropIndex(DropIndexClause {
                    name,
                    if_exists,
                    legacy_target: None,
                }))
            }
            TokenType::CONSTRAINT => {
                self.next_token();
                let (name, if_exists) = self.parse_drop_target("DROP CONSTRAINT")?;
                Ok(Clause::DropConstraint(DropConstraintClause {
                    name,
                    if_exists,
                }))
            }
            // ADR-0116 §2: `DROP DATABASE <name> [IF EXISTS] [<wait
            // clause>]`, `DROP COMPOSITE DATABASE ...` and `DROP ALIAS
            // <name> FOR DATABASE` are recognized (so they fail with CE's
            // `UnsupportedAdministrationCommand`, not a syntax error) but
            // never executed -- the namespaces are fixed. `DATABASE`/
            // `COMPOSITE`/`ALIAS` aren't reserved tokens, recognized by
            // spelling like `SHOW`/`USE` elsewhere in this parser.
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("database") => {
                self.unsupported_admin_clause(start)
            }
            TokenType::Identifier
                if tok.literal.eq_ignore_ascii_case("composite")
                    && self.peek_at_is_ident(1, "database") =>
            {
                self.unsupported_admin_clause(start)
            }
            TokenType::Identifier
                if tok.literal.eq_ignore_ascii_case("alias")
                    && self.peek_at(1).ty.is_ident_like()
                    && self.peek_at(2).ty == TokenType::FOR =>
            {
                self.unsupported_admin_clause(start)
            }
            _ => Err(self.error_at_token(
                &tok,
                format!(
                    "expected INDEX, CONSTRAINT, or DATABASE after DROP, got {:?}",
                    tok.literal
                ),
            )),
        }
    }

    /// The `[IF EXISTS] <name> [IF EXISTS]` target shared by `DROP INDEX`
    /// and `DROP CONSTRAINT`: `IF EXISTS` may precede or follow the name.
    /// Returns the name and whether `IF EXISTS` was written; `context`
    /// names the command in the missing-name error.
    pub(super) fn parse_drop_target(&mut self, context: &str) -> ParseResult<(SchemaName, bool)> {
        let leading_if_exists = self.parse_optional_if_exists()?;
        let name = self.parse_required_schema_name(context)?;
        let if_exists = leading_if_exists || self.parse_optional_if_exists()?;
        Ok((name, if_exists))
    }

    pub(super) fn parse_merge(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume MERGE

        let paths = self.parse_pattern()?;
        if paths.len() != 1 {
            return Err(self.error("MERGE requires exactly one pattern path"));
        }
        let pattern = paths.into_iter().next().unwrap();

        let mut on_create = Vec::new();
        let mut on_match = Vec::new();

        while self.peek_type() == TokenType::ON {
            self.next_token();
            let tok = self.next_token();
            match tok.ty {
                TokenType::CREATE => {
                    self.expect(TokenType::SET)?;
                    on_create = self.parse_set_items()?;
                }
                TokenType::MATCH => {
                    self.expect(TokenType::SET)?;
                    on_match = self.parse_set_items()?;
                }
                _ => {
                    return Err(self.error_at_token(
                        &tok,
                        format!("expected CREATE or MATCH after ON, got {:?}", tok.literal),
                    ));
                }
            }
        }

        Ok(Clause::Merge(MergeClause {
            pattern,
            on_create,
            on_match,
        }))
    }

    pub(super) fn parse_delete(&mut self, detach: bool) -> ParseResult<Clause> {
        self.next_token(); // consume DELETE
        let mut expressions = Vec::new();
        loop {
            let expr = self.parse_expression()?;
            expressions.push(expr);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(Clause::Delete(DeleteClause {
            detach,
            expressions,
        }))
    }

    /// CIP-199: consumes an optional `ALL` projection modifier directly
    /// after `RETURN`/`WITH` (explicit non-DISTINCT; semantically identical
    /// to omitting it -- `distinct: false` is already the default this is
    /// called from). Only fires when `ALL` is *not* immediately followed by
    /// `(`, since `ALL(` starts the pre-existing `all(x IN ... WHERE ...)`
    /// quantifier expression (parsed as the first return item, e.g.
    /// `RETURN all(x IN list WHERE x > 1)`) -- that usage must be left
    /// completely untouched. `RETURN ALL DISTINCT ...` deliberately isn't
    /// special-cased here: after consuming `ALL`, `DISTINCT` is not a valid
    /// return item, so `parse_return_items` naturally raises the syntax
    /// error the CIP calls for (ALL and DISTINCT are mutually exclusive).
    pub(super) fn consume_return_all_modifier(&mut self) {
        if self.all_is_set_quantifier() {
            self.next_token();
        }
    }

    /// `DISTINCT` directly after `RETURN`/`WITH` is the row modifier unless
    /// the token after it is `AS` — then it is an ordinary variable being
    /// aliased (`LET DISTINCT = 1 RETURN DISTINCT AS x`, Neo4j
    /// `LetAcceptance` [17]). Keyword priority: a name usable as an
    /// expression is read as one when the modifier reading cannot continue.
    fn distinct_is_modifier(&self) -> bool {
        self.peek_type() == TokenType::DISTINCT && self.peek_at(1).ty != TokenType::AS
    }

    /// The `DISTINCT` / `ALL` modifier after `RETURN` / `WITH`: true for
    /// `DISTINCT`, false for an explicit `ALL` or none.
    fn parse_distinct_modifier(&mut self) -> bool {
        if self.distinct_is_modifier() {
            self.next_token();
            true
        } else {
            self.consume_return_all_modifier();
            false
        }
    }

    /// The `ORDER BY` / `SKIP` (or `OFFSET`) / `LIMIT` tail the RETURN and
    /// WITH clauses share, in that order.
    fn parse_order_page(&mut self) -> ParseResult<(Vec<OrderByItem>, Option<Expr>, Option<Expr>)> {
        let order_by = if self.peek_type() == TokenType::ORDER {
            self.next_token();
            self.expect(TokenType::BY)?;
            self.parse_order_by()?
        } else {
            Vec::new()
        };

        let skip = if self.peek_type() == TokenType::SKIP || self.peek_is_offset() {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        let limit = if self.peek_type() == TokenType::LIMIT {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        Ok((order_by, skip, limit))
    }

    pub(super) fn parse_return(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume RETURN
        let distinct = self.parse_distinct_modifier();

        let items = self.parse_return_items()?;

        // CIP-236: the GROUP BY subclause sits between the return items and
        // the order-by/page clauses.
        let group_by = self.parse_group_by()?;

        let (order_by, skip, limit) = self.parse_order_page()?;

        Ok(Clause::Return(ReturnClause {
            group_by,
            distinct,
            items,
            order_by,
            skip,
            limit,
        }))
    }

    pub(super) fn parse_with(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume WITH
        let distinct = self.parse_distinct_modifier();

        let items = self.parse_return_items()?;

        // CIP-236 (canonical order: items, GROUP BY, order/page, WHERE).
        let group_by = self.parse_group_by()?;

        let (order_by, skip, limit) = self.parse_order_page()?;

        let where_expr = if self.peek_type() == TokenType::WHERE {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        Ok(Clause::With(WithClause {
            group_by,
            distinct,
            items,
            order_by,
            skip,
            limit,
            where_expr,
            origin: WithOrigin::Explicit,
        }))
    }

    /// CIP-236 `<group by clause>`: `GROUP BY <expr>...`, `GROUP BY ()`
    /// (empty grouping set), or `GROUP BY ALL`. `None` when the next token
    /// is not `GROUP`.
    pub(super) fn parse_group_by(&mut self) -> ParseResult<Option<GroupBySpec>> {
        if !self.peek_is_ident("GROUP") {
            return Ok(None);
        }
        self.next_token();
        // BY is a keyword token (ORDER BY), not an identifier.
        self.expect(TokenType::BY)?;
        // `()` is the empty grouping set only when it stands alone as the
        // clause tail -- `GROUP BY (expr)` is a parenthesized key, so require
        // the `()` pair *and* a clause/end token right after it.
        if self.peek_type() == TokenType::LeftParen
            && self.peek_at(1).ty == TokenType::RightParen
            && self.group_by_ends_clause(2)
        {
            self.next_token();
            self.next_token();
            return Ok(Some(GroupBySpec::Empty));
        }
        // ALL is a keyword token (RETURN ALL), not an identifier. Only when
        // it is *not* immediately followed by `(`, since `ALL(` is a function
        // call named `all` (an ordinary expression). Same rule as
        // `consume_return_all_modifier`.
        if self.all_is_set_quantifier() {
            self.next_token();
            return Ok(Some(GroupBySpec::All));
        }
        let mut keys = vec![self.parse_expression()?];
        while self.peek_type() == TokenType::Comma {
            self.next_token();
            keys.push(self.parse_expression()?);
        }
        Ok(Some(GroupBySpec::Keys(keys)))
    }

    /// True when the token `offset` ahead ends a `WITH`/`RETURN` clause (or
    /// the statement): nothing that could continue a grouping-key expression
    /// list follows. Disambiguates `GROUP BY ()` (the empty grouping set)
    /// from a parenthesized key expression.
    ///
    /// The clause half of the vocabulary is the one derived boundary
    /// predicate [`is_clause_boundary`] (token.rs; consulted here instead of
    /// a second hand-maintained list, after the round-5 LOAD/DROP drift): a
    /// new clause-start dispatch arm in `parse_clause_inner` extends its
    /// table and this walk sees it. The table's dispatch coverage is pinned
    /// by test (`clause_starter_table_covers_the_dispatch`).
    pub(super) fn group_by_ends_clause(&self, offset: usize) -> bool {
        matches!(
            self.peek_at(offset).ty,
            TokenType::EOF
                | TokenType::Semicolon
                | TokenType::RightParen
                | TokenType::RightBrace
                | TokenType::RETURN
        ) || is_clause_boundary(self.peek_at(offset).ty)
    }

    pub(super) fn parse_unwind(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume UNWIND
        let expr = self.parse_expression()?;
        self.expect(TokenType::AS)?;
        let alias = self.parse_alias_after_as()?;
        Ok(Clause::Unwind(UnwindClause {
            expr,
            alias,
            for_syntax: false,
        }))
    }

    /// CIP-250 (GQ10): `FOR <binding variable> IN <expression>` -- the
    /// GQL-compliant successor spelling of `UNWIND <expression> AS
    /// <binding variable>`, with the binding variable *before* the
    /// expression. Normalized here into the same [`Clause::Unwind`] the
    /// UNWIND parse produces (`for_syntax: true`), so the clause validates,
    /// lowers, plans and executes identically to UNWIND -- the CIP's own
    /// "no new logical plan operators" rule. UNWIND is not deprecated; the
    /// two spellings coexist. Default dialect since its graduation from
    /// the `gql-for` preview row (verified unlocked in live Neo4j 2026.09
    /// and 2026.10.1).
    pub(super) fn parse_for_clause(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume FOR
                           // The binding variable is a symbolic name, so any keyword spelling
                           // works unquoted too (ADR-0191), exactly like the FOREACH loop
                           // variable and the UNWIND alias.
        let alias = self.parse_schema_name("binding variable in FOR")?;
        self.expect(TokenType::IN)?;
        let expr = self.parse_expression()?;
        Ok(Clause::Unwind(UnwindClause {
            expr,
            alias,
            for_syntax: true,
        }))
    }

    pub(super) fn parse_foreach(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume FOREACH
        self.expect(TokenType::LeftParen)?;
        // The loop variable is a symbolic name, so any keyword spelling
        // works unquoted too (ADR-0191).
        let variable = self.parse_schema_name("variable in FOREACH")?;
        self.expect(TokenType::IN)?;
        let list_expr = self.parse_expression()?;
        self.expect(TokenType::Pipe)?;

        let mut clauses = Vec::new();
        while self.peek_type() != TokenType::RightParen {
            clauses.push(self.parse_clause()?);
        }
        self.next_token(); // consume )

        Ok(Clause::Foreach(ForeachClause {
            variable,
            list_expr,
            clauses,
        }))
    }

    pub(super) fn parse_load_csv(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume LOAD
        let csv_tok = self.next_token();
        if !csv_tok.literal.eq_ignore_ascii_case("CSV") {
            return Err(self.error_at_token(
                &csv_tok,
                format!("expected CSV after LOAD, got {:?}", csv_tok.literal),
            ));
        }

        let with_headers = if self.peek_type() == TokenType::WITH {
            self.next_token();
            let headers_tok = self.next_token();
            if !headers_tok.literal.eq_ignore_ascii_case("HEADERS") {
                return Err(self.error_at_token(
                    &headers_tok,
                    format!("expected HEADERS after WITH, got {:?}", headers_tok.literal),
                ));
            }
            true
        } else {
            false
        };

        self.expect(TokenType::FROM)?;
        let source = self.parse_expression()?;
        self.expect(TokenType::AS)?;
        let alias = self.parse_alias_after_as()?;

        let field_terminator = if self.peek_is_ident(crate::token::FIELDTERMINATOR_WORD) {
            self.next_token();
            let ft_tok = self.next_token();
            if ft_tok.ty != TokenType::String {
                return Err(self.error_at_token(
                    &ft_tok,
                    format!(
                        "expected a string literal after FIELDTERMINATOR, got {:?}",
                        ft_tok.literal
                    ),
                ));
            }
            Some(ft_tok.literal.into_owned())
        } else {
            None
        };

        Ok(Clause::LoadCSV(LoadCSVClause {
            with_headers,
            source,
            alias,
            field_terminator,
        }))
    }

    /// The side of the label-mixing split the clause at the cursor feeds:
    /// read for MATCH/WITH/RETURN (and the other expression-bearing read
    /// clauses), write for CREATE/INSERT, both for MERGE (which reads and
    /// writes), and nothing for the label specifications SET/REMOVE validate
    /// separately and for the clauses that carry no pattern labels.
    fn label_mix_target_for_current_clause(&self) -> LabelMixTarget {
        match self.peek_type() {
            TokenType::CREATE | TokenType::INSERT => LabelMixTarget::Write,
            TokenType::MERGE => LabelMixTarget::Both,
            TokenType::SET
            | TokenType::REMOVE
            | TokenType::DELETE
            | TokenType::DETACH
            | TokenType::FOREACH
            | TokenType::CALL
            | TokenType::LOAD => LabelMixTarget::None,
            TokenType::OPTIONAL => match self.peek_at(1).ty {
                TokenType::MATCH => LabelMixTarget::Read,
                _ => LabelMixTarget::None,
            },
            _ => LabelMixTarget::Read,
        }
    }

    /// The clause-level label-mixing rejections (CIP-40 Section 3.6,
    /// CIP-14): mixing colon-conjunction with a GQL label-expression symbol
    /// or with the `IS` introducer in one scope. The read and write sides
    /// are checked independently — Neo4j keeps them apart across the clauses
    /// of one subquery scope. The `mixed-label-syntax` preview opt-in
    /// suspends both.
    fn check_label_mixing(&self, flags: &PatternSideEffectFlags) -> ParseResult<()> {
        if self.options.mixed_label_syntax {
            return Ok(());
        }
        if let Some(message) = flags.read_labels.mixing() {
            return Err(self.error(message));
        }
        if let Some(message) = flags.write_labels.mixing() {
            return Err(self.error(message));
        }
        Ok(())
    }
}

mod call;
mod match_clause;
mod set_remove;
mod tail;
