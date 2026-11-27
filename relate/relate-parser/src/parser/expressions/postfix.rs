//! Arithmetic precedence levels, unary operators and the postfix chain (property access, subscripts, calls).

use super::*;
use crate::parser::operators::{CONCAT, DIV, MINUS, MOD, MUL, PLUS, POW};

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_add_sub(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_mul_div()?;
        loop {
            match self.peek_type() {
                TokenType::Plus => {
                    self.next_token();
                    let right = self.parse_mul_div()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: PLUS,
                        right,
                    }));
                }
                TokenType::Dash => {
                    self.next_token();
                    let right = self.parse_mul_div()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: MINUS,
                        right,
                    }));
                }
                TokenType::DoublePipe => {
                    self.next_token();
                    let right = self.parse_mul_div()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: CONCAT,
                        right,
                    }));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    pub(in crate::parser) fn parse_mul_div(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_exponentiation()?;
        loop {
            match self.peek_type() {
                TokenType::Star => {
                    self.next_token();
                    let right = self.parse_exponentiation()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: MUL,
                        right,
                    }));
                }
                TokenType::Slash => {
                    self.next_token();
                    let right = self.parse_exponentiation()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: DIV,
                        right,
                    }));
                }
                TokenType::Percent => {
                    self.next_token();
                    let right = self.parse_exponentiation()?;
                    left = Expr::Binary(Box::new(BinaryExpr {
                        left,
                        op: MOD,
                        right,
                    }));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    pub(in crate::parser) fn parse_exponentiation(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_unary()?;
        while self.peek_type() == TokenType::Caret {
            self.next_token();
            let right = self.parse_unary()?;
            left = Expr::Binary(Box::new(BinaryExpr {
                left,
                op: POW,
                right,
            }));
        }
        Ok(left)
    }

    pub(in crate::parser) fn parse_unary(&mut self) -> ParseResult<Expr> {
        if self.peek_type() == TokenType::Plus {
            // ADR-0191 pins the operator spelling `RETURN NOT + 1` as a
            // syntax error (KeywordSymbolicNames [34]); keep that pin while
            // a unary plus elsewhere is accepted (Neo4j's `RETURN +1`).
            if self.pos > 0 && self.tokens[self.pos - 1].ty == TokenType::NOT {
                let tok = self.peek().clone();
                return Err(
                    self.error_at_token(&tok, format!("unexpected token \"{}\"", tok.literal))
                );
            }
            // Unary plus is the identity on numbers (Neo4j's `RETURN +1`),
            // but a directly nested unary plus (`+ +1`, and `++1`) is an
            // invalid expression start in the reference engine, so this is
            // the length-two prefix we do not accept.
            if self.peek_at(1).ty == TokenType::Plus {
                let bad = self.peek_at(1).clone();
                return Err(
                    self.error_at_token(&bad, format!("unexpected token \"{}\"", bad.literal))
                );
            }
            self.next_token();
            let expr = self.with_expr_depth(|p| p.parse_unary())?;
            return Ok(Expr::Unary(Box::new(UnaryExpr { op: PLUS, expr })));
        }
        if self.peek_type() == TokenType::Dash {
            // A double (or longer) unary minus applied directly to an
            // Infinity/Inf/NaN literal (no parens in between) is a compile-time
            // syntax error in Neo4j, even though `-Infinity` and
            // `-(-Infinity)` are both valid (CIP-55 deviation: `--Infinity` is
            // rejected while `--1` still parses). Detect a run of two or more
            // leading Dash tokens landing directly on such a literal.
            if self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1].ty == TokenType::Dash {
                let mut lookahead = self.pos + 1;
                while lookahead < self.tokens.len() && self.tokens[lookahead].ty == TokenType::Dash
                {
                    lookahead += 1;
                }
                if lookahead < self.tokens.len()
                    && matches!(
                        self.tokens[lookahead].ty,
                        TokenType::INFINITY | TokenType::NAN
                    )
                {
                    let bad_tok = self.tokens[lookahead].clone();
                    return Err(self.error_at_token(
                        &bad_tok,
                        format!("Invalid input '{}', expected: a number", bad_tok.literal),
                    ));
                }
            }
            self.next_token();
            // Special case: -<integer literal> for int64 min value
            if self.peek_type() == TokenType::Integer {
                let next = self.peek().clone();
                if parse_int_literal(&next.literal).is_err() {
                    let neg_literal = format!("-{}", next.literal);
                    if let Ok(val) = parse_int_literal(&neg_literal) {
                        self.next_token();
                        return Ok(Expr::Literal(LiteralValue::Integer(val)));
                    }
                }
            }
            // ADR-0257: unary `-` chains recurse here directly, bypassing
            // `parse_expression`, so each step must count separately
            // (LABS-69).
            let expr = self.with_expr_depth(|p| p.parse_unary())?;
            return Ok(Expr::Unary(Box::new(UnaryExpr { op: MINUS, expr })));
        }
        self.parse_postfix()
    }

    /// Walks the maximal `.ident` token chain starting at `from` (which must
    /// point at a `Dot`) and reports `(end, ends_in_call)`: `end` is the
    /// token index where the scan stopped — the `(` when the chain leads to
    /// a function call, otherwise the first token that broke the chain — and
    /// `ends_in_call` tells which. Pure lookahead over the token stream;
    /// consumes nothing. The result is constant for every dot position
    /// inside one maximal chain, which is what lets `parse_postfix` memoize
    /// it instead of rescanning the remaining tokens per dot.
    pub(in crate::parser) fn scan_dot_chain_for_call(&self, from: usize) -> (usize, bool) {
        let mut pos = from;
        while pos < self.tokens.len() && self.tokens[pos].ty == TokenType::Dot {
            let ident = pos + 1;
            if ident >= self.tokens.len() || !self.tokens[ident].ty.is_schema_name() {
                return (ident, false);
            }
            let after = pos + 2;
            if after < self.tokens.len() && self.tokens[after].ty == TokenType::LeftParen {
                return (after, true);
            }
            if after >= self.tokens.len() || self.tokens[after].ty != TokenType::Dot {
                return (after, false);
            }
            pos = after;
        }
        (pos, false)
    }

    /// With `PATH [ first` consumed, whether the bracket opens the path
    /// constructor rather than a subscript. A comma can never follow a
    /// subscript index, so `PATH [a, r, b]` is the constructor. A
    /// one-element `PATH [x]` is the constructor only under
    /// `path-construction` and when `path` is bound nowhere in the statement
    /// (the subscript reading could only have been an undefined variable).
    fn path_constructor_follows(&self) -> bool {
        match self.peek_type() {
            TokenType::Comma => true,
            TokenType::RightBracket => {
                self.options.path_construction && self.path_ident_never_bound()
            }
            _ => false,
        }
    }

    /// The element list of a path constructor whose `first` element is
    /// parsed, through the closing `]` (ADR-0637).
    fn parse_path_constructor_rest(&mut self, first: Expr) -> ParseResult<Vec<Expr>> {
        let mut elements = vec![first];
        while self.peek_type() == TokenType::Comma {
            self.next_token(); // consume ,
            elements.push(self.parse_expression()?);
        }
        self.expect(TokenType::RightBracket)?;
        Ok(elements)
    }

    pub(in crate::parser) fn parse_postfix(&mut self) -> ParseResult<Expr> {
        let primary = self.parse_primary()?;
        let ns = Self::extract_namespace(&primary);
        let mut base = PostfixBase::Built(primary, ns);
        // Memoized `scan_dot_chain_for_call` result: `(scan end, result)`.
        // Query positions (the dot after each consumed `.ident`) are strictly
        // increasing and the result is constant within one maximal chain, so
        // a cached `(end, res)` answers every later dot of the same chain
        // (`pos < end`) — each token is scanned at most once per
        // `parse_postfix` call rather than an O(remaining) lookahead per dot.
        let mut chain_scan: Option<(usize, bool)> = None;
        loop {
            match self.peek_type() {
                TokenType::Dot => {
                    self.next_token();
                    let prop_name = self.parse_schema_name("property name after '.'")?;
                    // Check for namespaced function call: expr.name(args) or expr.name.name(args)
                    if self.peek_type() == TokenType::LeftParen {
                        // Use the namespace path collected so far.
                        let path = match &mut base {
                            PostfixBase::Built(_, ns) => ns.take(),
                            PostfixBase::NsPath(path) => Some(std::mem::take(path)),
                        };
                        if let Some(mut func_name) = path {
                            func_name.push('.');
                            prop_name.with_str(|seg| func_name.push_str(seg));
                            self.next_token(); // consume (
                            let args = self.parse_arg_list()?;
                            base = PostfixBase::Built(
                                Expr::FunctionCall(FunctionCallExpr {
                                    name: SmallName::from_name(&func_name),
                                    args,
                                    distinct: false,
                                }),
                                None,
                            );
                            continue;
                        }
                    }
                    // Check for deeper nesting: if next is '.', this could be a longer namespace
                    // Only use namespace Variable if the chain eventually leads to a function call '('
                    if self.peek_type() == TokenType::Dot {
                        let carries_path = matches!(
                            &base,
                            PostfixBase::Built(_, Some(_)) | PostfixBase::NsPath(_)
                        );
                        if carries_path {
                            // Look ahead to see if this chain eventually leads to '('
                            let is_func_namespace = match chain_scan {
                                Some((end, res)) if self.pos < end => res,
                                _ => {
                                    let scanned = self.scan_dot_chain_for_call(self.pos);
                                    chain_scan = Some(scanned);
                                    scanned.1
                                }
                            };
                            if is_func_namespace {
                                // Accumulate the segment into the bare path,
                                // entering (or staying in) `NsPath`:
                                // materializing an `Expr` per segment cost
                                // Θ(n²) in string clones (a parse-time DoS on
                                // maximal inputs), and nothing can observe the
                                // intermediate value — the terminating `(` arm
                                // above rebuilds the callee name from the path.
                                match &mut base {
                                    PostfixBase::Built(_, ns) => {
                                        if let Some(mut path) = ns.take() {
                                            path.push('.');
                                            prop_name.with_str(|seg| path.push_str(seg));
                                            base = PostfixBase::NsPath(path);
                                            continue;
                                        }
                                    }
                                    PostfixBase::NsPath(path) => {
                                        path.push('.');
                                        prop_name.with_str(|seg| path.push_str(seg));
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    // Plain property access; extend the running namespace path
                    // to keep it in sync (`extract_namespace` of a Property is
                    // the base namespace plus `.` plus the property).
                    base = match base {
                        PostfixBase::Built(expr, mut ns) => {
                            if let Some(ns_str) = ns.as_mut() {
                                ns_str.push('.');
                                prop_name.with_str(|seg| ns_str.push_str(seg));
                            }
                            PostfixBase::Built(
                                Expr::Property(Box::new(PropertyAccess {
                                    expr,
                                    property: prop_name,
                                    key_expr: None,
                                })),
                                ns,
                            )
                        }
                        // Unreachable for valid input: the scan admitted
                        // `NsPath` only for a chain ending in `(`, so
                        // mid-chain the token after `.ident` is always `.`
                        // or `(` — never a plain property hop. A violation
                        // is a typed internal error, never a panic.
                        PostfixBase::NsPath(_) => {
                            return Err(self.internal_error(
                                "a call-bound dot-chain only continues at '.' or '('",
                            ));
                        }
                    };
                }
                TokenType::LeftBracket => {
                    // Subscript results are never namespace paths. `NsPath`
                    // is unreachable here for valid input by the same scan
                    // invariant as the plain-property arm above; a violation
                    // is a typed internal error, never a panic.
                    let PostfixBase::Built(expr, _) = base else {
                        return Err(self.internal_error(
                            "a call-bound dot-chain only continues at '.' or '('",
                        ));
                    };
                    // The constructor's `[` directly follows a bare `PATH`
                    // variable: `x.PATH [`, `(PATH) [` and a second bracket
                    // all fail one of the two tests.
                    let is_path_open =
                        matches!(expr, Expr::Variable(_)) && self.is_path_opener_at(self.pos - 1);
                    self.next_token(); // consume [
                                       // Check for [..to]
                    if self.peek_type() == TokenType::DotDot {
                        self.next_token(); // consume ..
                        let to = self.parse_expression()?;
                        self.expect(TokenType::RightBracket)?;
                        base = PostfixBase::Built(
                            Expr::Subscript(Box::new(SubscriptExpr {
                                expr,
                                index: None,
                                from: None,
                                to: Some(to),
                                slice: true,
                            })),
                            None,
                        );
                        continue;
                    }
                    let first = self.parse_expression()?;
                    if is_path_open && self.path_constructor_follows() {
                        let elements = self.parse_path_constructor_rest(first)?;
                        base = PostfixBase::Built(Expr::PathConstruct(elements), None);
                        continue;
                    }
                    if self.peek_type() == TokenType::DotDot {
                        self.next_token(); // consume ..
                        let to = if self.peek_type() != TokenType::RightBracket {
                            Some(self.parse_expression()?)
                        } else {
                            None
                        };
                        self.expect(TokenType::RightBracket)?;
                        base = PostfixBase::Built(
                            Expr::Subscript(Box::new(SubscriptExpr {
                                expr,
                                index: None,
                                from: Some(first),
                                to,
                                slice: true,
                            })),
                            None,
                        );
                    } else {
                        self.expect(TokenType::RightBracket)?;
                        base = PostfixBase::Built(
                            Expr::Subscript(Box::new(SubscriptExpr {
                                expr,
                                index: Some(first),
                                from: None,
                                to: None,
                                slice: false,
                            })),
                            None,
                        );
                    }
                }
                // Note: the `variable:<label-expr>` predicate used to be
                // parsed here (in `<postfix operator>`, tighter than `+`).
                // CIP-190 moves it to `parse_string_null_check`'s `<advanced
                // comparison predicate part 2>` level (looser than `+`,
                // alongside the `IS <label-expr>` spelling) -- see
                // label-expression-precedence.feature. `parse_postfix` no
                // longer consumes a bare `:` at all, so it correctly stops
                // and lets the caller's `<advanced comparison predicate part
                // 2>` check see it.
                TokenType::LeftBrace
                    if matches!(&base, PostfixBase::Built(Expr::Variable(_), _))
                        && (self.peek_brace_is_map_projection()
                            // `n {}` is an empty map projection. The
                            // subquery-introducer primaries (EXISTS/COUNT/
                            // COLLECT/CASE) keep their own empty-brace
                            // rejection, so this arm is only reached for an
                            // ordinary bound variable.
                            || self.peek_at(1).ty == TokenType::RightBrace) =>
                {
                    // `<variable> <map projection>` (openCypher's map
                    // projection is always rooted at a plain variable).
                    // Also how CIP-190's near-ambiguities resolve
                    // `exists {name: exists.name}` / `case {name:
                    // case.name}` in favor of the ordinary-variable
                    // reading once the EXISTS/COUNT/COLLECT/CASE primary
                    // handling has already decided the keyword isn't
                    // introducing a subquery/CASE expression here.
                    //
                    // Gated on the same `peek_brace_is_map_projection`
                    // lookahead used for EXISTS/COUNT/COLLECT: without it,
                    // this would also fire for CIP-146's `USE <graph-expr>
                    // { <query> }` top-level brace body (whose graph-expr
                    // parsing runs through this same general expression
                    // grammar) and try to parse the query body as map
                    // projection items, hard-erroring instead of leaving
                    // the `{` for `parse_braced_single_query`.
                    //
                    // A map projection is never a namespace path. The
                    // non-`Built` case is unreachable for valid input (the
                    // match guard above only matches a built variable); a
                    // violation is a typed internal error, never a panic.
                    let PostfixBase::Built(expr, _) = base else {
                        return Err(self.internal_error("the guard only matches a built variable"));
                    };
                    let items = self.parse_map_projection_items()?;
                    base = PostfixBase::Built(
                        Expr::MapProjection(Box::new(MapProjectionExpr { expr, items })),
                        None,
                    );
                }
                _ => break,
            }
        }
        match base {
            PostfixBase::Built(expr, _) => Ok(expr),
            // A call-bound dot-chain is exited only through its terminating
            // `(` arm, which leaves a `Built` base — so `NsPath` here is
            // unreachable for valid input; a violation is a typed internal
            // error, never a panic.
            PostfixBase::NsPath(_) => {
                Err(self.internal_error("a call-bound dot-chain ends at its '(' arm"))
            }
        }
    }
}
