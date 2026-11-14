use super::*;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_set(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume SET
        let items = self.parse_set_items()?;
        Ok(Clause::Set(SetClause { items }))
    }

    pub(in crate::parser) fn parse_set_items(&mut self) -> ParseResult<Vec<SetItem>> {
        let mut items = Vec::new();
        loop {
            items.push(self.parse_set_item()?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(items)
    }

    /// Parses zero or more trailing SET/REMOVE-target accessors after a base
    /// expression: `.prop` (static) or `[<expr>]` (CIP-131 dynamic). Used by
    /// both `parse_set_item` and `parse_remove_item` so `n.a.b`, `n[a][b]`,
    /// `n.a[b]`, `(expr)[a].b`, etc. all parse uniformly -- all but the last
    /// accessor navigate to the entity ultimately mutated (see
    /// `build_property_access`).
    pub(in crate::parser) fn parse_property_target_accessors(
        &mut self,
    ) -> ParseResult<Vec<PropertyTargetAccessor>> {
        let mut accessors = Vec::new();
        loop {
            match self.peek_type() {
                TokenType::Dot => {
                    self.next_token(); // consume .
                    let prop = self.parse_schema_name("property name")?;
                    accessors.push(PropertyTargetAccessor::Static(prop));
                }
                TokenType::LeftBracket => {
                    self.next_token(); // consume [
                    let key_expr = self.parse_expression()?;
                    self.expect(TokenType::RightBracket)?;
                    accessors.push(PropertyTargetAccessor::Dynamic(key_expr));
                }
                _ => break,
            }
        }
        Ok(accessors)
    }

    /// Folds a base expression and a non-empty accessor chain into the
    /// `PropertyAccess` a SET/REMOVE property item targets: all but the last
    /// accessor become part of the entity-navigation expression (nested
    /// `Expr::Property`/`Expr::Subscript`), and the last accessor becomes the
    /// actual static or dynamic (CIP-131) property target.
    pub(in crate::parser) fn build_property_access(
        base: Expr,
        mut accessors: Vec<PropertyTargetAccessor>,
    ) -> PropertyAccess {
        let last = accessors.pop().expect("accessors must be non-empty");
        let mut entity_expr = base;
        for accessor in accessors {
            entity_expr = match accessor {
                PropertyTargetAccessor::Static(name) => Expr::Property(Box::new(PropertyAccess {
                    expr: entity_expr,
                    property: name,
                    key_expr: None,
                })),
                PropertyTargetAccessor::Dynamic(key_expr) => {
                    Expr::Subscript(Box::new(SubscriptExpr {
                        expr: entity_expr,
                        index: Some(key_expr),
                        from: None,
                        to: None,
                        slice: false,
                    }))
                }
            };
        }
        match last {
            PropertyTargetAccessor::Static(name) => PropertyAccess {
                expr: entity_expr,
                property: name,
                key_expr: None,
            },
            PropertyTargetAccessor::Dynamic(key_expr) => PropertyAccess {
                expr: entity_expr,
                property: SmallName::from_name(""),
                key_expr: Some(key_expr),
            },
        }
    }

    /// Whether the current token starts a CIP-131 general property target —
    /// the base forms [`Self::parse_general_property_target`] accepts: a
    /// call/identifier followed by `(` (`coalesce(n, m)[c]`), a `[`-rooted
    /// comprehension target, a parameter (`$node['k']`), and the literal
    /// bases `null`/`true`/`false`/`CASE` (`REMOVE null.p`,
    /// `SET CASE … END.age`). SET and REMOVE read the same production, so
    /// they must classify its start with this one predicate; two token lists
    /// had made `REMOVE null.p` parse while `SET null.p = 1` fell through to
    /// a variable read.
    pub(in crate::parser) fn starts_general_property_target(&self) -> bool {
        (self.peek_type() == TokenType::Identifier && self.peek_at(1).ty == TokenType::LeftParen)
            || matches!(
                self.peek_type(),
                TokenType::LeftBracket
                    | TokenType::Parameter
                    | TokenType::NULL
                    | TokenType::TRUE
                    | TokenType::FALSE
                    | TokenType::CASE
            )
    }

    /// Parses a SET/REMOVE property target whose base is neither the plain
    /// `<variable>` form nor a parenthesized `(<expr>)` form -- e.g.
    /// `coalesce(n, m)[c]` (CIP-131 basic-syntax.feature [14]: an
    /// unparenthesized complex target is valid, unlike the doc's implication
    /// that parens are required -- see that CIP's NOTES.md deviation #1).
    /// Reuses the general postfix expression grammar (`parse_postfix`, the
    /// same one that parses `n.prop` / `n[expr]` on the read side, including
    /// chained accessors) so the whole target parses identically to the read
    /// path, then peels off the outermost accessor as the actual target
    /// (mirroring `build_property_access`'s last-accessor rule). Note
    /// `parse_postfix`'s `:` (HasLabel) handling only fires when its operand
    /// is *currently* a bare `Expr::Variable`, so it never fires here for a
    /// function-call/complex base -- no conflict with SET/REMOVE's own
    /// `n:Label` syntax, which is parsed by the caller before falling back
    /// to this method.
    pub(in crate::parser) fn parse_general_property_target(
        &mut self,
    ) -> ParseResult<PropertyAccess> {
        let expr = self.parse_postfix()?;
        match expr {
            Expr::Property(pa) => Ok(*pa),
            Expr::Subscript(se) if !se.slice && se.index.is_some() => Ok(PropertyAccess {
                expr: se.expr,
                property: SmallName::from_name(""),
                key_expr: se.index,
            }),
            _ => Err(self.error(
                "expected a '.' or '[' property target after the SET/REMOVE target expression",
            )),
        }
    }

    pub(in crate::parser) fn parse_set_item(&mut self) -> ParseResult<SetItem> {
        // Handle parenthesized expression: SET (n).prop = value / SET (n)[c] = value
        if self.peek_type() == TokenType::LeftParen {
            self.next_token(); // consume (
            let inner_expr = self.parse_expression()?;
            self.expect(TokenType::RightParen)?;

            let accessors = self.parse_property_target_accessors()?;
            if accessors.is_empty() {
                let next = self.peek().clone();
                return Err(self.error_at_token(
                    &next,
                    format!(
                        "expected '.' or '[' after parenthesized expression in SET, got {:?}",
                        next.literal
                    ),
                ));
            }
            let prop_access = Self::build_property_access(inner_expr, accessors);
            self.expect(TokenType::Equals)?;
            let val = self.parse_expression()?;
            return Ok(SetItem {
                property: Some(prop_access),
                variable: None,
                labels: Vec::new(),
                value: Some(val),
                merge: false,
                is_keyword: false,
                labels_function: None,
            });
        }

        // CIP-162 Alternatives: `SET labels(<expr>) = / += / -= <expr>`
        // (deferred by the CIP itself but implemented here as its stated
        // target semantics -- see `SetItem::labels_function`'s doc
        // comment). Must be tried before the CIP-131 general complex-target
        // branch below, since `labels(...)` also matches
        // `Identifier LeftParen` -- backtracks to that branch if the tail
        // after `labels(<expr>)` isn't actually `=`/`+=`/`-=` (e.g.
        // `labels(n)[0] = ...`, an ordinary dynamic-property-key target).
        if self.peek_is_ident("labels") && self.peek_at(1).ty == TokenType::LeftParen {
            let saved = self.save_pos();
            self.next_token(); // consume 'labels'
            self.next_token(); // consume (
            let target = self.parse_expression()?;
            let op = if self.expect(TokenType::RightParen).is_ok() {
                match self.peek_type() {
                    TokenType::Equals => {
                        self.next_token();
                        Some(LabelsFunctionOp::Overwrite)
                    }
                    TokenType::Plus if self.peek_at(1).ty == TokenType::Equals => {
                        self.next_token();
                        self.next_token();
                        Some(LabelsFunctionOp::Extend)
                    }
                    TokenType::Dash if self.peek_at(1).ty == TokenType::Equals => {
                        self.next_token();
                        self.next_token();
                        Some(LabelsFunctionOp::Remove)
                    }
                    _ => None,
                }
            } else {
                None
            };
            match op {
                Some(op) => {
                    let val = self.parse_expression()?;
                    return Ok(SetItem {
                        property: None,
                        variable: None,
                        labels: Vec::new(),
                        value: Some(val),
                        merge: false,
                        is_keyword: false,
                        labels_function: Some((target, op)),
                    });
                }
                None => self.restore_pos(saved),
            }
        }

        // CIP-131: an unparenthesized complex target expression, e.g.
        // `SET coalesce(n, m)[c] = ...` -- the base isn't a plain variable
        // (it's a function call here), so it can't be the `n:Label` /
        // `n = ...` / `n += ...` / `n.prop = ...` / `n[...] = ...` forms
        // below, all of which start with a bare identifier NOT immediately
        // followed by `(`. A target may also start with `[`: the list
        // comprehension/static-property form `SET [x IN ns WHERE x.p = 1][0].p = -1`
        // (TCK Set7 [8]), which is likewise no plain variable. The literal
        // bases are the same production REMOVE reads (`SET null.p = 1`),
        // classified by the one predicate.
        if self.starts_general_property_target() {
            let prop_access = self.parse_general_property_target()?;
            self.expect(TokenType::Equals)?;
            let val = self.parse_expression()?;
            return Ok(SetItem {
                property: Some(prop_access),
                variable: None,
                labels: Vec::new(),
                value: Some(val),
                merge: false,
                is_keyword: false,
                labels_function: None,
            });
        }

        // ADR-0191 (Decision point 7's follow-up probe): Neo4j 2026.09
        // accepts every keyword spelling as a SET target variable
        // (`SET MATCH.prop = 1`, `SET MATCH:Label`, `SET MATCH += {...}`),
        // so this reads through the shared symbolic-name read like every
        // other variable slot.
        let name = self.parse_schema_name("identifier in SET")?;

        // CIP-131: `SET n.prop = ...` / `SET n[<expr>] = ...`, including
        // chained accessors (`n[a][b]`, `n[a].b`, ...) where all but the
        // last accessor navigate to the entity being mutated.
        let accessors = self.parse_property_target_accessors()?;
        if !accessors.is_empty() {
            let prop_access = Self::build_property_access(Expr::Variable(name), accessors);
            self.expect(TokenType::Equals)?;
            let val = self.parse_expression()?;
            return Ok(SetItem {
                property: Some(prop_access),
                variable: None,
                labels: Vec::new(),
                value: Some(val),
                merge: false,
                is_keyword: false,
                labels_function: None,
            });
        }

        let next = self.peek().clone();
        match next.ty {
            TokenType::Colon | TokenType::IS => {
                let (labels, is_keyword) = self.parse_label_set_list()?;
                Ok(SetItem {
                    property: None,
                    variable: Some(name),
                    labels,
                    value: None,
                    merge: false,
                    is_keyword,
                    labels_function: None,
                })
            }
            TokenType::Equals => {
                self.next_token();
                let val = self.parse_expression()?;
                Ok(SetItem {
                    property: None,
                    variable: Some(name),
                    labels: Vec::new(),
                    value: Some(val),
                    merge: false,
                    is_keyword: false,
                    labels_function: None,
                })
            }
            TokenType::Plus => {
                self.next_token(); // consume +
                self.expect(TokenType::Equals)?;
                let val = self.parse_expression()?;
                Ok(SetItem {
                    property: None,
                    variable: Some(name),
                    labels: Vec::new(),
                    value: Some(val),
                    merge: true,
                    is_keyword: false,
                    labels_function: None,
                })
            }
            _ => Err(self.error_at_token(
                &next,
                format!("unexpected token {:?} in SET clause", next.literal),
            )),
        }
    }

    /// Parses a SET/REMOVE label-set item's labels after the variable
    /// (CIP-126): `:A` / `:A:B:C` (colon, chainable) or `IS A` (IS keyword,
    /// single label only). Each label atom may also be a CIP-162 dynamic
    /// label (`$(<expr>)`/`$all(<expr>)`/`$any(<expr>)`), chainable the same
    /// way (`n:$(a):$(b)`, `n IS $(a)`). Label-expression symbols (`&`, `|`,
    /// `!`, `%`) are never allowed here, and `IS` cannot be chained with `:`
    /// within one item. Returns the flat label-atom list and whether `IS`
    /// was used (for the CIP-40/CIP-126 statement-level IS/colon-conjunction
    /// mixing check).
    pub(in crate::parser) fn parse_label_set_list(
        &mut self,
    ) -> ParseResult<(Vec<LabelExpr>, bool)> {
        let via_is = self.peek_type() == TokenType::IS;
        self.next_token(); // consume ':' or 'IS'
        if via_is && self.peek_is_labeled_keyword() {
            return Err(self.error(
                "invalid label specification context: IS LABELED is a predicate, not a label specification",
            ));
        }
        self.reject_label_spec_symbols()?;
        let mut labels = vec![self.parse_label_atom()?];
        if via_is {
            if self.peek_type() == TokenType::Colon {
                return Err(self.error(
                    "MixedIsAndColonConjunction: cannot mix IS and colon-conjunction within the same label item",
                ));
            }
        } else {
            while self.peek_type() == TokenType::Colon {
                self.next_token();
                self.reject_label_spec_symbols()?;
                labels.push(self.parse_label_atom()?);
            }
        }
        self.reject_label_spec_symbols()?;
        Ok((labels, via_is))
    }

    /// Fails if the current token is a label-expression symbol, which a
    /// SET/REMOVE label specification never allows (see
    /// `parse_label_set_list`).
    pub(in crate::parser) fn reject_label_spec_symbols(&self) -> ParseResult<()> {
        if matches!(
            self.peek_type(),
            TokenType::Ampersand | TokenType::Pipe | TokenType::Bang | TokenType::Percent
        ) {
            return Err(self.error(
                "InvalidLabelExpression: label expression symbols (&, |, !, %) are not allowed in a SET/REMOVE label specification",
            ));
        }
        Ok(())
    }

    pub(in crate::parser) fn parse_remove(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume REMOVE
        let mut items = Vec::new();
        loop {
            items.push(self.parse_remove_item()?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(Clause::Remove(RemoveClause { items }))
    }

    pub(in crate::parser) fn parse_remove_item(&mut self) -> ParseResult<RemoveItem> {
        // Handle a parenthesized expression property target (CIP-118 static
        // `.prop`, CIP-131 dynamic `[<expr>]`): `REMOVE (<expr>).prop` /
        // `REMOVE (<expr>)[<expr>]`, e.g. `REMOVE (COLLECT {...}[0]).prop`.
        // Per the CIP's revision history, label removal still requires a
        // plain variable -- only property removal accepts a general
        // expression -- so `(expr):Label` is intentionally not accepted
        // here (falls through to a syntax error).
        if self.peek_type() == TokenType::LeftParen {
            self.next_token(); // consume (
            let inner_expr = self.parse_expression()?;
            self.expect(TokenType::RightParen)?;
            let accessors = self.parse_property_target_accessors()?;
            if accessors.is_empty() {
                let next = self.peek().clone();
                return Err(self.error_at_token(
                    &next,
                    format!(
                        "expected '.' or '[' after parenthesized expression in REMOVE, got {:?}",
                        next.literal
                    ),
                ));
            }
            return Ok(RemoveItem {
                property: Some(Self::build_property_access(inner_expr, accessors)),
                variable: None,
                labels: Vec::new(),
                is_keyword: false,
            });
        }

        // CIP-131: an unparenthesized complex target expression, e.g.
        // `REMOVE coalesce(n, m)[c]` or a list comprehension's element
        // `REMOVE [x IN ns WHERE x.p = 1][0].p` (see `parse_set_item`'s
        // matching comment). The one start-token predicate SET also uses.
        if self.starts_general_property_target() {
            let prop_access = self.parse_general_property_target()?;
            return Ok(RemoveItem {
                property: Some(prop_access),
                variable: None,
                labels: Vec::new(),
                is_keyword: false,
            });
        }

        // ADR-0191 (Decision point 7's follow-up probe): Neo4j 2026.09
        // accepts every keyword spelling as a REMOVE target variable
        // (`REMOVE MATCH.prop`, `REMOVE MATCH:Label`), matching
        // `parse_set_item`'s equivalent read.
        let name = self.parse_schema_name("identifier in REMOVE")?;

        // CIP-131: `REMOVE n.prop` / `REMOVE n[<expr>]`, including chained
        // accessors (see `parse_set_item`'s matching comment).
        let accessors = self.parse_property_target_accessors()?;
        if !accessors.is_empty() {
            let prop_access = Self::build_property_access(Expr::Variable(name), accessors);
            return Ok(RemoveItem {
                property: Some(prop_access),
                variable: None,
                labels: Vec::new(),
                is_keyword: false,
            });
        }

        let next = self.peek().clone();
        match next.ty {
            TokenType::Colon | TokenType::IS => {
                let (labels, is_keyword) = self.parse_label_set_list()?;
                Ok(RemoveItem {
                    property: None,
                    variable: Some(name),
                    labels,
                    is_keyword,
                })
            }
            _ => Err(self.error_at_token(
                &next,
                format!(
                    "expected . or : after identifier in REMOVE, got {:?}",
                    next.literal
                ),
            )),
        }
    }
}
