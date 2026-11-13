use super::*;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse(&mut self) -> ParseResult<Statement> {
        let preamble = self.parse_preamble()?;

        let mut stmt = self.parse_statement_block()?;
        stmt.preamble = preamble;

        if self.peek_type() == TokenType::Semicolon {
            self.next_token();
        }
        // Any token left unconsumed here means `parse_regular_query`
        // returned without absorbing the rest of the input -- e.g. a bare
        // `WHEN ... THEN ...` query (CIP-135) cannot be UNIONed (syntax_placement.feature
        // scenarios 16/17), and two brace-enclosed `<single query>`s
        // juxtaposed with no `UNION`/`NEXT` connector between them
        // (scenario 12/13) leave the second `{...}` sitting here. Every
        // other clause-loop path already consumes all the way to EOF on
        // its own (`parse_statement`'s loop keeps calling `parse_clause`
        // until it hits one of EOF/`;`/`UNION`/`}`, and any clause-shaped
        // leftover token already produces its own "unexpected token" error
        // there), so this is specifically the top-level backstop for the
        // brace-form `SingleQuery`/`<when query>` productions, which don't
        // loop.
        if self.peek_type() != TokenType::EOF {
            return Err(self.error(format!("unexpected token {:?}", self.peek().literal)));
        }

        Ok(stmt)
    }

    /// CIP-150: consumes the optional statement preamble at the very start
    /// of the query -- any interleaving of `EXPLAIN`/`PROFILE` (each at
    /// most once, mutually exclusive) with zero or more `CYPHER [<version>]
    /// [<key>=<value>]...` clauses. `CYPHER` is a non-reserved word --
    /// recognized by spelling on a plain `Identifier` token, like `SHOW`/
    /// `USE` elsewhere -- checked only here, once, at the true start of
    /// input, so it never shadows a variable named `cypher` appearing
    /// anywhere else in a query.
    ///
    /// Grammar (per the CIP): `<preamble> ::= <preamble option>...`,
    /// `<preamble option> ::= EXPLAIN | PROFILE | <cypher options>`,
    /// `<cypher options> ::= CYPHER [<cypher version>] <cypher option>...`.
    /// A fresh `CYPHER` keyword is required to open each options clause --
    /// once one ends (at the first token that isn't a recognized
    /// `<key>=<value>` pair), only another `CYPHER`/`EXPLAIN`/`PROFILE`
    /// token can continue the preamble; anything else (including a bare
    /// `key=value` with no preceding fresh `CYPHER`) falls through to
    /// ordinary statement parsing and is a generic syntax error there --
    /// this is what makes `CYPHER 25 EXPLAIN runtime=slotted RETURN 1`
    /// a hard error: `EXPLAIN` ends the `CYPHER` clause, and
    /// `runtime=slotted` isn't a valid statement start.
    ///
    /// One Cypher version is supported: 25 (ADR-0584). A `CYPHER 25`
    /// preamble is an accepted no-op -- an unprefixed statement already
    /// speaks Cypher 25, so the version is never recorded and the
    /// statement lowers exactly as the same query without it; repeating
    /// the same version is fine. Every other `CYPHER <n>` integer,
    /// `CYPHER 5` included, fails with the one authored
    /// unsupported-version error naming the requested value, while a
    /// non-integer version token stays the 22N10 grammar rejection. Two
    /// different versions for one statement cannot occur (only 25
    /// parses); two different values for the same non-version option key
    /// are rejected as a conflicting pre-parser combination.
    pub(in crate::parser) fn parse_preamble(&mut self) -> ParseResult<PreambleMode> {
        let mut options: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // One mode, starting at `Execute`: each preamble keyword is
        // admitted only from `Execute`, so a second `EXPLAIN` or `PROFILE`
        // ends the preamble and the leftover keyword becomes the top-level
        // parse's unexpected-token error -- the mode can never hold both.
        let mut mode = PreambleMode::Execute;

        loop {
            match self.peek_type() {
                TokenType::EXPLAIN if mode == PreambleMode::Execute => {
                    self.next_token();
                    mode = PreambleMode::Explain;
                }
                TokenType::PROFILE if mode == PreambleMode::Execute => {
                    self.next_token();
                    mode = PreambleMode::Profile;
                }
                TokenType::Identifier if self.peek().literal.eq_ignore_ascii_case("cypher") => {
                    self.next_token(); // consume CYPHER

                    // Optional `<cypher version> ::= <digit>...` -- only
                    // attempted when the next token could plausibly be one
                    // (an integer or decimal digit sequence); anything else
                    // (an option key, EXPLAIN, EOF, ...) means no version
                    // was given at all, per the grammar's `[<cypher
                    // version>]`.
                    if matches!(self.peek_type(), TokenType::Integer | TokenType::Float) {
                        let tok = self.next_token();
                        // A non-integer version token stays the 22N10
                        // grammar rejection; an integer other than the
                        // supported one is ADR-0584's authored
                        // unsupported-version error naming the requested
                        // value. `CYPHER 25` itself is the accepted
                        // no-op, and the version is never recorded -- an
                        // unprefixed statement already speaks Cypher 25,
                        // so the preamble selects nothing. The comparison
                        // is the token's canonical decimal text, so a
                        // radix spelling of the same value is still the
                        // rejection.
                        if tok.ty != TokenType::Integer {
                            return Err(self.error_at_token(
                                &tok,
                                format!(
                                    "SyntaxError: 22N10 invalid pre-parser option value: {:?} is not a valid cypher version",
                                    tok.literal
                                ),
                            ));
                        }
                        if tok.literal != SUPPORTED_CYPHER_VERSION.to_string() {
                            return Err(self.error_at_token(
                                &tok,
                                crate::error::unsupported_cypher_version_message(&tok.literal),
                            ));
                        }
                    }

                    // Zero or more `<cypher option key> = <cypher option
                    // value>` pairs, each `<key>` a plain identifier
                    // directly followed by `=`.
                    while self.peek_type() == TokenType::Identifier
                        && self.peek_at(1).ty == TokenType::Equals
                    {
                        let key_tok = self.next_token();
                        self.next_token(); // consume =
                        let value_tok = self.next_token();
                        let key_lower = key_tok.literal.to_ascii_lowercase();
                        if !CYPHER_OPTION_KEYS.contains(&key_lower.as_str()) {
                            return Err(self.error_at_token(
                                &key_tok,
                                format!(
                                    "SyntaxError: 22N07 invalid pre-parser option key: {:?}",
                                    key_tok.literal
                                ),
                            ));
                        }
                        // Keys with a constrained value vocabulary
                        // (`CYPHER_OPTION_VALUES`) validate their value
                        // the same way -- an authored 22N10 error naming
                        // the valid set, the value-side twin of the 22N07
                        // key error above.
                        if let Some((_, values)) = CYPHER_OPTION_VALUES
                            .iter()
                            .find(|(key, _)| *key == key_lower)
                        {
                            if !values.contains(&value_tok.literal.to_ascii_lowercase().as_str()) {
                                return Err(self.error_at_token(
                                    &value_tok,
                                    format!(
                                        "SyntaxError: 22N10 invalid pre-parser option value: {:?} is not a valid {} value (valid values: {})",
                                        value_tok.literal,
                                        key_tok.literal,
                                        values.join(", ")
                                    ),
                                ));
                            }
                        }
                        match options.get(&key_lower) {
                            Some(existing) if existing != &value_tok.literal => {
                                return Err(self.error_at_token(
                                    &value_tok,
                                    format!(
                                        "SyntaxError: 22N09 conflicting pre-parser combination: multiple values for {:?}",
                                        key_tok.literal
                                    ),
                                ));
                            }
                            _ => {
                                options.insert(key_lower, value_tok.literal.into_owned());
                            }
                        }
                    }
                }
                _ => break,
            }
        }

        Ok(mode)
    }

    /// `<RegularQuery> ::= <SingleQuery> (UNION [ALL] <SingleQuery>)*` -- a
    /// single query optionally chained to further queries via UNION/UNION
    /// ALL. This is the top-level query grammar minus EXPLAIN/PROFILE (which
    /// only apply once, at the very start of a top-level query), so it is
    /// also the grammar every subquery body uses: `COUNT { ... }`,
    /// `EXISTS { ... }`, `COLLECT { ... }`, and `CALL { ... }` all embed a
    /// `<RegularQuery>` and must therefore accept a UNION chain between their
    /// braces, not just a single statement.
    ///
    /// CIP-146: each single query is now parsed via `parse_single_query`,
    /// which additionally accepts the brace-enclosed
    /// `[ USE <graph-expr> ] '{' <RegularQuery> '}'` form. Per the CIP's
    /// Semantics Step 3 (Unwrap), a brace-enclosed arm is transparent
    /// (Case 1) when it is the *sole* `SingleQuery` here (no `UNION`
    /// follows) -- its clauses/union chain are simply used as this
    /// `RegularQuery` directly, which is exactly what `parse_single_query`
    /// already returns. But once a `UNION` token *does* follow, every arm
    /// (including this first one) is now a `SingleQuery`-only/UNION-argument
    /// context (Case 2): an arm whose own brace body was itself composite
    /// (nested UNION) needs the `CALL (*) { BODY } RETURN *` / `... FINISH`
    /// rewrap (Case 2.2) via `maybe_wrap_composite_union_arm` so the outer
    /// union's dedup boundary lands exactly where the braces put it (see
    /// union-braces.feature scenario 7); a linear arm needs no rewrap since
    /// splicing it in directly is already observably identical.
    pub(in crate::parser) fn parse_regular_query(&mut self) -> ParseResult<Statement> {
        // CIP-135: `<regular query> ::= <union query> | <when query> |
        // <single query>` -- a `<when query>` (bare `WHEN ... THEN {...}
        // [WHEN ...]* [ELSE {...}]`) is a distinct top-level alternative,
        // not composable with UNION and not a `<single query>` (so it
        // can't appear as a UNION arm) unless explicitly wrapped in
        // top-level braces (`{ WHEN ... }`, which *is* a `<single query>`
        // per CIP-146 since its content is a full `<regular query>`).
        // Only checked here, at the very start of a `<regular query>` --
        // never inside `parse_clause_inner`, so a `WHEN` appearing after
        // an earlier clause in the same statement (e.g. `MATCH (n) WHEN
        // ...`) still falls through to that dispatch's catch-all
        // "unexpected token" error.
        if self.peek_type() == TokenType::WHEN {
            return self.parse_when_query();
        }
        let mut stmt = self.parse_single_query()?;

        if self.peek_type() != TokenType::UNION {
            return Ok(stmt);
        }
        stmt = Self::maybe_wrap_composite_union_arm(stmt);

        // UNION / UNION ALL / UNION DISTINCT (CIP-154: `DISTINCT` is an
        // optional synonym for plain UNION's set semantics -- mutually
        // exclusive with `ALL`, and represented identically to plain UNION
        // as `all: false`; mixing `UNION` with `UNION ALL` in one chain is
        // accepted, as in Neo4j).
        while self.peek_type() == TokenType::UNION {
            self.next_token();
            let all = if self.peek_type() == TokenType::ALL {
                self.next_token();
                true
            } else {
                if self.peek_type() == TokenType::DISTINCT {
                    self.next_token();
                }
                false
            };
            let next_stmt = self.parse_single_query()?;
            let next_stmt = Self::maybe_wrap_composite_union_arm(next_stmt);
            // Attach to end of union chain
            let mut current = &mut stmt;
            while current.union.is_some() {
                current = &mut current.union.as_mut().unwrap().statement;
            }
            current.union = Some(Box::new(UnionPart {
                all,
                statement: next_stmt,
            }));
        }

        Ok(stmt)
    }

    /// True if the current token is the CIP-213 `NEXT` contextual keyword --
    /// a non-reserved word (per the CIP's own grammar), recognized by
    /// spelling on a plain `Identifier` token, the same technique already
    /// used for `SHOW`/`USE`/`LET`/`FILTER`/`FINISH`. Only ever checked at
    /// clause-list-loop boundaries (`parse_statement`'s stop condition) and
    /// at the top of `parse_statement_block`'s own loop -- never inside
    /// `parse_clause_inner` -- so `next` remains usable as an ordinary
    /// variable/label name everywhere else (e.g. `MATCH (next) RETURN
    /// next`).
    pub(in crate::parser) fn at_next_keyword(&self) -> bool {
        self.peek_type() == TokenType::Identifier
            && self.peek().literal.eq_ignore_ascii_case("next")
    }

    /// CIP-213 `<statement block> ::= <statement> [<next statement>...]`,
    /// `<next statement> ::= NEXT <statement>`. Wraps `parse_regular_query`
    /// (a `<statement>`: a possibly-UNION'd `<composite statement>`, or a
    /// bare `<conditional statement>` i.e. a `<when query>`) with a
    /// trailing loop over `NEXT <statement>` stages, threading each stage
    /// onto the previous one's `.next` field. This is the production every
    /// "full regular query" call site now uses in place of a raw
    /// `parse_regular_query` call, so NEXT is parseable everywhere a
    /// `<RegularQuery>` embeds (top level, brace-enclosed nested procedure
    /// specifications, CALL subquery bodies, and EXISTS/COUNT/COLLECT
    /// subquery-expression bodies) -- matching every context the CIP's own
    /// examples chain NEXT through.
    ///
    /// ADR-0257: this is the single choke point through which EVERY
    /// statement-nesting cycle funnels -- CALL/EXISTS/COUNT/COLLECT
    /// subquery bodies, brace-enclosed nested procedure specifications
    /// (`parse_braced_single_query`), and WHEN/ELSE bodies
    /// (`parse_when_query` via `parse_single_query`). The braced/WHEN
    /// cycles recurse `parse_single_query` -> `parse_braced_single_query`
    /// -> here without passing through `parse_statement`, `parse_clause`,
    /// or `parse_expression`, so none of the other depth counters engages
    /// on them (LABS-69: ~3000 nested braces on a ~6 KB input used to
    /// SIGABRT on an 8 MiB release stack). Guarding here with the
    /// dedicated `block_depth` counter -- deliberately *not*
    /// `statement_depth`, which doubles as the `is_outer_clause`
    /// label-mixing signal and must not be inflated by a top-level
    /// `{ ... }` body -- bounds every cycle at the same one-increment-per-
    /// nesting-level rate.
    pub(in crate::parser) fn parse_statement_block(&mut self) -> ParseResult<Statement> {
        self.with_block_depth(Self::parse_statement_block_at_current_depth)
    }

    pub(in crate::parser) fn parse_statement_block_at_current_depth(
        &mut self,
    ) -> ParseResult<Statement> {
        if self.at_next_keyword() {
            return Err(
                self.error("InvalidNextStatement: NEXT must follow a statement, not begin one")
            );
        }
        // Every "full regular query" embedding context (CALL/EXISTS/COUNT/
        // COLLECT bodies, brace-enclosed nested procedure specifications)
        // starts a *fresh* scope for `in_when_branch_body` -- that flag
        // only exists to let a bare (non-brace) WHEN/THEN body's own
        // clause loop stop at a sibling `WHEN`, and must not leak into an
        // unrelated nested body parsed while it happens to be set (e.g. a
        // `CALL { ... }` subquery embedded inside a bare THEN body, whose
        // own bare `MATCH (n) WHEN ...` must still be a hard error per
        // CIP-135, not silently treated as a statement-block boundary).
        let saved_in_when_branch_body = std::mem::take(&mut self.in_when_branch_body);
        // CIP-231: the optional `<definition block>` heads every
        // `<procedure specification>` -- top level and every braced nested
        // body (this is the one choke point all of them funnel through, so
        // definitions parse in exactly the grammar's positions and nowhere
        // else). Parsed before the statement proper; the resolver in the
        // front end rewrites definitions and their call sites away before
        // lowering.
        let definitions = self.parse_local_definitions()?;
        let result = self.parse_statement_block_inner();
        self.in_when_branch_body = saved_in_when_branch_body;
        let mut result = result?;
        // `extend`, not assign: a block that starts with its own DEFINE
        // statements can still parse into a braced inner body carrying
        // definitions of its own (`DEFINE ... { DEFINE ... ... }`); the
        // outer block's own (possibly empty) definitions come first in
        // declaration order, the inner body's after them.
        result.definitions.splice(0..0, definitions);
        Ok(result)
    }

    pub(in crate::parser) fn parse_statement_block_inner(&mut self) -> ParseResult<Statement> {
        let mut stmt = self.parse_regular_query()?;
        // `stmt` may already carry its own `.next` chain -- e.g. a
        // brace-enclosed nested procedure specification
        // (`parse_braced_single_query`) whose own content is itself a
        // multi-stage NEXT chain (CIP-213). Find the *true* current
        // tail before appending anything more, so an outer NEXT here
        // extends that chain instead of clobbering it.
        let mut tail = &mut stmt;
        while tail.next.is_some() {
            tail = tail.next.as_mut().unwrap();
        }
        while self.at_next_keyword() {
            self.next_token(); // consume NEXT
            if self.at_next_keyword() {
                return Err(self.error(
                    "InvalidNextStatement: NEXT cannot be immediately followed by another NEXT",
                ));
            }
            if matches!(
                self.peek_type(),
                TokenType::EOF | TokenType::Semicolon | TokenType::RightBrace | TokenType::UNION
            ) {
                return Err(
                    self.error("InvalidNextStatement: NEXT must be followed by a statement")
                );
            }
            let next_stmt = self.parse_regular_query()?;
            tail.next = Some(Box::new(next_stmt));
            tail = tail.next.as_mut().unwrap();
        }
        Ok(stmt)
    }

    /// CIP-146 Semantics Step 3, Case 2.2: rewrites a composite (nested-union)
    /// UNION argument `arm` -- which can only arise from a brace-enclosed
    /// body whose own content was itself a UNION chain -- into
    /// `CALL (*) { arm } RETURN *` (arm ends in a tabular RETURN) or
    /// `CALL (*) { arm } FINISH` (arm's result is omitted, e.g. every leaf
    /// arm is CREATE-only). A linear `arm` (`.union.is_none()`) is returned
    /// unchanged, since splicing it directly into the outer chain is already
    /// observably identical to wrapping it.
    ///
    /// CIP-213: also fires when `arm.next.is_some()` -- a brace-enclosed
    /// body whose own content is a NEXT chain (PE4.5/PE4.6). Without this,
    /// attaching a sibling UNION arm directly onto `arm` would leave `arm`
    /// carrying *both* `.union` (the sibling arm) and `.next` (arm's own
    /// internal continuation) on the same node, and `Engine::
    /// execute_statement_seeded` -- which always resolves `.union` before
    /// `.next` -- would then wrongly feed the *whole merged* union row set
    /// (both arms' rows) into `arm`'s own internal NEXT stage instead of
    /// just its own branch's rows. Wrapping `arm` (chain and all) inside an
    /// opaque `CALL (*) { ... }` first makes it a single self-contained,
    /// `.next`-free node before any sibling UNION arm is ever attached to
    /// it, so the two concepts never collide.
    pub(in crate::parser) fn maybe_wrap_composite_union_arm(arm: Statement) -> Statement {
        if arm.union.is_none() && arm.next.is_none() {
            return arm;
        }
        let mut tail = &arm;
        while let Some(next_stmt) = &tail.next {
            tail = next_stmt;
        }
        let tabular = tail.clauses.iter().any(|c| matches!(c, Clause::Return(_)));
        let trailer = if tabular {
            Clause::Return(ReturnClause {
                group_by: None,
                distinct: false,
                items: vec![ReturnItem {
                    expr: Expr::Star,
                    alias: None,
                    all: true,
                    source_text: None,
                }],
                order_by: Vec::new(),
                skip: None,
                limit: None,
            })
        } else {
            Clause::Finish
        };
        Statement {
            clauses: vec![
                Clause::CallSubquery(CallSubqueryClause {
                    statement: arm,
                    scope: CallScope::Wildcard,
                    optional: false,
                    in_transactions: None,
                }),
                trailer,
            ],
            union: None,
            preamble: PreambleMode::Execute,
            next: None,
            brace_wrapped: false,
            definitions: Vec::new(),
        }
    }

    /// `<SingleQuery> ::= <Statement> | [ USE <graph-expr> ] '{' <RegularQuery> '}'`
    /// (CIP-146). The plain form is unchanged pre-CIP-146 behavior
    /// (delegated to `parse_statement`); the brace form is new. Used
    /// everywhere a `SingleQuery` is expected: the first arm of a
    /// `RegularQuery`, every subsequent UNION/UNION ALL argument, and (since
    /// `CALL`/`CALL (...)` bodies are themselves a `RegularQuery`) the top
    /// level of a CALL subquery body -- exactly the "known to process one
    /// row" contexts the CIP restricts this syntax to. Deliberately *not*
    /// wired into `parse_clause_inner`, so brace syntax stays rejected mid-
    /// query (e.g. after WITH) and as a LOAD CSV argument body.
    pub(in crate::parser) fn parse_single_query(&mut self) -> ParseResult<Statement> {
        if self.peek_type() == TokenType::LeftBrace {
            return self.parse_braced_single_query(None);
        }
        if self.peek_type() == TokenType::Identifier
            && self.peek().literal.eq_ignore_ascii_case("use")
        {
            // Could be the new `USE <graph-expr> { ... }` brace-prefixed
            // SingleQuery, or an ordinary leading `USE` clause of a plain
            // statement (pre-CIP-146, e.g. `USE db RETURN 1`) -- parse the
            // USE clause (it already knows how to parse a graph expression)
            // and check what follows before committing.
            let save = self.save_pos();
            let use_clause = self.parse_use()?;
            if self.peek_type() == TokenType::LeftBrace {
                return self.parse_braced_single_query(Some(use_clause.graph_expr));
            }
            self.restore_pos(save);
        }
        self.parse_statement()
    }

    /// Parses the body of a CIP-146 `[ USE <graph-expr> ] '{' <RegularQuery> '}'`
    /// SingleQuery; `use_prefix` is the already-parsed USE graph expression
    /// (if any) -- the leading `{` is the current token. Empty braces are a
    /// syntax error (grammar requires at least one SingleQuery inside).
    /// Nested braces (Semantics Step 1, Simplify) collapse for free via
    /// plain recursion into `parse_regular_query`, which itself dispatches
    /// back through `parse_single_query` for its own arms. A USE prefix is
    /// pushed (Semantics Step 2, Normalize) as the first clause of every arm
    /// of the resulting body -- linear body: just the one statement;
    /// composite body: every arm of its union chain.
    pub(in crate::parser) fn parse_braced_single_query(
        &mut self,
        use_prefix: Option<Expr>,
    ) -> ParseResult<Statement> {
        let brace_tok = self.next_token(); // consume {
        if self.peek_type() == TokenType::RightBrace {
            return Err(self.error_at_token(
                &brace_tok,
                "InvalidInput: empty braces `{ }` are not a valid query body",
            ));
        }
        let mut inner = self.parse_statement_block()?;
        self.expect(TokenType::RightBrace)?;
        inner.brace_wrapped = true;

        if let Some(graph_expr) = use_prefix {
            let mut cur = &mut inner;
            loop {
                cur.clauses.insert(
                    0,
                    Clause::Use(UseClause {
                        graph_expr: graph_expr.clone(),
                    }),
                );
                if cur.union.is_some() {
                    cur = &mut cur.union.as_mut().unwrap().statement;
                } else {
                    break;
                }
            }
        }

        Ok(inner)
    }

    /// CIP-135: `<when query> ::= <when>, {<when>}, [<else>]`, where
    /// `<when> ::= "WHEN", <predicate>, "THEN", <single-query>` and
    /// `<else> ::= "ELSE", <single-query>`. Called from `parse_regular_query`
    /// once it has confirmed the current token is `WHEN`. Each branch body
    /// (and the optional ELSE body) is parsed via `parse_single_query`, so
    /// it may be a top-level-brace `{ ... }` body (the only form every
    /// scenario in this CIP's eval suite actually uses) or a bare
    /// single-/multi-part query per the grammar. Result is wrapped as the
    /// sole clause of a fresh `Statement` -- a `<when query>` never mixes
    /// with other clauses and (being a `<regular query>`-level alternative,
    /// not a `<single query>`) is deliberately not connected to a following
    /// `UNION` here; callers that need brace+UNION composition already get
    /// it for free via `parse_braced_single_query`'s recursive call into
    /// this same function.
    pub(in crate::parser) fn parse_when_query(&mut self) -> ParseResult<Statement> {
        let mut branches = Vec::new();
        loop {
            self.expect(TokenType::WHEN)?;
            let predicate = self.parse_expression()?;
            self.expect(TokenType::THEN)?;
            let body = self.parse_when_branch_body()?;
            branches.push(WhenBranch { predicate, body });
            if self.peek_type() != TokenType::WHEN {
                break;
            }
        }
        let else_body = if self.peek_type() == TokenType::ELSE {
            self.next_token();
            Some(self.parse_when_branch_body()?)
        } else {
            None
        };
        Ok(Statement {
            clauses: vec![Clause::Conditional(ConditionalClause {
                branches,
                else_body,
            })],
            ..Statement::default()
        })
    }

    /// Parses one WHEN or ELSE branch body with `in_when_branch_body`
    /// raised, restoring it before any error propagates (same balanced
    /// shape as the `with_*_depth` helpers).
    pub(in crate::parser) fn parse_when_branch_body(&mut self) -> ParseResult<Statement> {
        self.in_when_branch_body += 1;
        let body = self.parse_single_query();
        self.in_when_branch_body -= 1;
        body
    }

    pub(in crate::parser) fn parse_statement(&mut self) -> ParseResult<Statement> {
        // ADR-0257: pure statement recursion (`CALL { CALL { ... } }`,
        // `EXISTS { EXISTS { ... } }`) must hit the same hard nesting cap as
        // expression recursion instead of overflowing the stack (LABS-69).
        // Routed through the balanced helper so an error mid-body cannot
        // leave `statement_depth` inflated for a speculative re-parse.
        //
        // This counter does NOT engage on every statement-nesting cycle:
        // `parse_braced_single_query` and `parse_when_query` bodies recurse
        // `parse_single_query` -> `parse_braced_single_query` ->
        // `parse_statement_block` -> ... without passing through
        // `parse_statement` (or `parse_clause`/`parse_expression`). Those
        // cycles are now guarded by `block_depth` at `parse_statement_block`
        // -- the choke point every statement-nesting cycle funnels through.
        // The hard cap here is kept anyway, belt-and-braces: on any stack,
        // every active `parse_statement` frame sits inside a distinct
        // enclosing `parse_statement_block` frame (its only caller,
        // `parse_single_query`, is reached only via `parse_regular_query` /
        // `parse_when_query`, both of which run inside a block), so
        // `block_depth >= statement_depth` always holds and `block_depth`
        // provably trips at the same-or-earlier nesting depth on every
        // cycle this cap catches -- but keeping it costs nothing and
        // protects against a future caller path that bypasses
        // `parse_statement_block`. The counter itself is load-bearing
        // regardless of the cap: it is the `is_outer_clause` label-mixing
        // scoping signal (see `parse_clause`).
        self.with_statement_depth(Self::parse_statement_at_current_depth)
    }

    pub(in crate::parser) fn parse_statement_at_current_depth(&mut self) -> ParseResult<Statement> {
        let mut stmt = Statement {
            clauses: Vec::new(),
            ..Statement::default()
        };

        // See `parse_clause`'s doc comment for how label-expression mixing
        // is scoped relative to this statement-nesting depth.
        loop {
            match self.peek_type() {
                TokenType::EOF
                | TokenType::Semicolon
                | TokenType::UNION
                | TokenType::RightBrace
                // CIP-135: a bare (non-brace) WHEN/THEN body -- e.g.
                // `WHEN a = 1 THEN RETURN 2 AS a ELSE RETURN 3 AS a` --
                // must stop before the following `ELSE`, the same way a
                // brace already stops it, rather than trying to parse
                // `ELSE` as the start of a new clause. `WHEN` is
                // deliberately *not* added here (see `parse_regular_query`'s
                // doc comment): a bare `WHEN` appearing after an ordinary
                // clause must remain a hard parse error, not silently
                // become a new statement-block boundary.
                | TokenType::ELSE => {
                    if self.peek_type() == TokenType::Semicolon {
                        self.next_token();
                    }
                    break;
                }
                TokenType::WHEN if self.in_when_branch_body > 0 => break,
                // CIP-213: `NEXT` is a statement-block boundary, not a
                // clause -- stop the clause loop here so the enclosing
                // `parse_statement_block` can consume it. No real clause
                // ever starts with the bare word "next", so this can never
                // misfire mid-clause (see `at_next_keyword`'s doc comment).
                _ if self.at_next_keyword() => break,
                _ => {
                    let clause = self.parse_clause()?;
                    stmt.clauses.push(clause);
                }
            }
        }

        self.check_label_set_mixing(&stmt.clauses)?;
        // CIP-123/CIP-242/CIP-253: composed SHOW/TERMINATE clauses (multiple
        // SHOW/TERMINATE commands chained together, or a single SHOW/
        // TERMINATE composed with other Cypher clauses) are now genuinely
        // supported -- each acts as a nested-loop cross join against the
        // rows produced so far, exactly like `SHOW FUNCTIONS`/`SHOW
        // PROCEDURES` already did before this feature landed (see
        // `crate::clause::show`). The structural rules that govern
        // composition (mandatory non-star YIELD, mandatory trailing RETURN,
        // no re-declaring an in-scope variable) are checked at the semantic
        // (post-parse) validation stage instead of here, in
        // `relate_front::validate::validate_show_composability` --
        // this parser only builds the AST.

        Ok(stmt)
    }

    /// CIP-126: within a single SET clause (or a single REMOVE
    /// clause), `IS` and a colon-conjunction (`:A:B`, i.e. more than one
    /// label in one item) cannot both be used across that one clause's
    /// label-set items -- single-label items of either syntax may freely
    /// coexist. Checked independently per clause (CIP-126
    /// `05-invalid-mixing.feature` never combines items across two
    /// different clauses; CIP-40 `dml-label-set-mixing.feature` scenario 14
    /// is `@neo4j-verified` legal for exactly that cross-clause shape: a
    /// SET clause using `IS` and a *separate* REMOVE clause using colon
    /// conjunction, both in the same statement).
    pub(in crate::parser) fn check_label_set_mixing(&self, clauses: &[Clause]) -> ParseResult<()> {
        for clause in clauses {
            let items: Vec<(bool, usize)> = match clause {
                Clause::Set(sc) => sc
                    .items
                    .iter()
                    .filter(|i| !i.labels.is_empty())
                    .map(|i| (i.is_keyword, i.labels.len()))
                    .collect(),
                Clause::Remove(rc) => rc
                    .items
                    .iter()
                    .filter(|i| !i.labels.is_empty())
                    .map(|i| (i.is_keyword, i.labels.len()))
                    .collect(),
                _ => continue,
            };
            let mut saw_is = false;
            let mut saw_colon_conjunction = false;
            for (is_keyword, len) in items {
                if is_keyword {
                    saw_is = true;
                } else if len > 1 {
                    saw_colon_conjunction = true;
                }
            }
            if saw_is && saw_colon_conjunction {
                return Err(self.error(
                    "MixedIsAndColonConjunction: cannot mix IS and colon-conjunction across SET/REMOVE label items in the same clause",
                ));
            }
        }
        Ok(())
    }
}
