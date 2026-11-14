//! Graph-type DDL parsing: `CREATE`/`ALTER CURRENT GRAPH TYPE` bodies, element
//! and constraint specifications, and the inline-constraint forms.

use super::*;

impl<'a> Parser<'a> {
    /// `ALTER CURRENT GRAPH TYPE (SET|ADD|ALTER|DROP) '{' <graph type
    /// specification body> '}'` (CIP-176/182). `SET`/`DROP` are reserved
    /// keywords (`TokenType::SET`/`TokenType::DROP`) everywhere else, so
    /// they're matched directly; `ADD`/`ALTER` are not reserved and are
    /// recognized by spelling like `SHOW`/`USE` elsewhere in this parser.
    pub(in crate::parser) fn parse_alter_current_graph_type(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume ALTER
        self.expect_keyword_ident("CURRENT")?;
        self.expect_keyword_ident("GRAPH")?;
        self.expect_keyword_ident("TYPE")?;
        let verb = if self.peek_type() == TokenType::SET {
            self.next_token();
            // ADR-0211: `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>`
            // promotes a `CREATE GRAPH TYPE`-declared named type to current,
            // instead of taking a `{ <body> }`.
            if self.peek_is_ident("graph") && self.peek_at_is_ident(1, "type") {
                self.next_token(); // GRAPH
                self.next_token(); // TYPE
                let name = self.parse_bare_name("identifier")?;
                return Ok(Clause::AlterCurrentGraphType(AlterCurrentGraphTypeClause {
                    verb: AlterGraphTypeVerb::Set,
                    target: AlterCurrentGraphTypeTarget::Named(name),
                }));
            }
            AlterGraphTypeVerb::Set
        } else if self.peek_type() == TokenType::DROP {
            self.next_token();
            AlterGraphTypeVerb::Drop
        } else if self.peek_is_ident("add") {
            self.next_token();
            AlterGraphTypeVerb::Add
        } else if self.peek_is_ident("alter") {
            self.next_token();
            AlterGraphTypeVerb::Alter
        } else {
            let tok = self.peek().clone();
            return Err(self.error_at_token(
                &tok,
                format!("expected SET, ADD, ALTER, or DROP, got {:?}", tok.literal),
            ));
        };
        // CIP-182: only a `DROP` body's grammar allows the bare `CONSTRAINT
        // <name>` (no `FOR ...`) drop-by-name element, and a `DROP` body
        // drops a constraint by name only.
        let (open, body) = self.parse_graph_type_spec_body()?;
        if verb == AlterGraphTypeVerb::Drop {
            self.reject_undesignated_constraint_drops(&open, &body)?;
        } else {
            self.reject_bare_constraint_drops(&open, &body)?;
        }
        Ok(Clause::AlterCurrentGraphType(AlterCurrentGraphTypeClause {
            verb,
            target: AlterCurrentGraphTypeTarget::Body(body),
        }))
    }

    /// `CREATE GRAPH TYPE <name> [AS] '{' <graph type specification body>
    /// '}'` (CIP-178 grammar; the optional `AS` is CIP-180's spelling of
    /// the same body-introducing form). Not a reserved keyword sequence --
    /// `parse_create`'s dispatch recognizes `GRAPH TYPE` by spelling before
    /// calling this.
    pub(in crate::parser) fn parse_create_graph_type(&mut self) -> ParseResult<Clause> {
        self.expect_keyword_ident("GRAPH")?;
        self.expect_keyword_ident("TYPE")?;
        let name = self.parse_bare_name("identifier")?;
        let via_as = self.peek_type() == TokenType::AS;
        if via_as {
            self.next_token();
        }
        let (open, body) = self.parse_graph_type_spec_body()?;
        self.reject_bare_constraint_drops(&open, &body)?;
        Ok(Clause::CreateGraphType(CreateGraphTypeClause {
            name,
            body,
            via_as,
        }))
    }

    /// `'{' [ <graph type spec element> (',' <graph type spec element>)*
    /// ] '}'` (`<graph type specification body>`). Grammar-only:
    /// see `GraphTypeBody`'s doc comment for why the CIP-178 semantic
    /// rules live elsewhere. Returns the consumed opening `{` with the body,
    /// the token `reject_bare_constraint_drops` and
    /// `reject_undesignated_constraint_drops` errors point at.
    pub(in crate::parser) fn parse_graph_type_spec_body(
        &mut self,
    ) -> ParseResult<(Token<'a>, GraphTypeBody)> {
        let open = self.expect(TokenType::LeftBrace)?;
        let mut body = GraphTypeBody::default();
        if self.peek_type() != TokenType::RightBrace {
            loop {
                self.parse_graph_type_element(&mut body)?;
                if self.peek_type() == TokenType::Comma {
                    self.next_token();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenType::RightBrace)?;
        Ok((open, body))
    }

    /// Rejects a parsed `body` holding a bare `CONSTRAINT <name>`
    /// drop-by-name element: only an `ALTER CURRENT GRAPH TYPE DROP` body's
    /// grammar has one, every other body's `CONSTRAINT` needs its `FOR ...`
    /// specification. The error points at `open`, the body's `{`.
    pub(in crate::parser) fn reject_bare_constraint_drops(
        &self,
        open: &Token,
        body: &GraphTypeBody,
    ) -> ParseResult<()> {
        match body.drop_constraint_names.first() {
            None => Ok(()),
            Some(name) => Err(self.error_at_token(
                open,
                format!(
                    "constraint {name:?} has no FOR specification; only ALTER CURRENT GRAPH TYPE DROP names a constraint without one"
                ),
            )),
        }
    }

    /// Rejects an `ALTER CURRENT GRAPH TYPE DROP` `body` holding an
    /// undesignated (unnamed) `CONSTRAINT FOR ...` specification: such a
    /// constraint can only be dropped by name. A
    /// standalone specification's reference always has an alias
    /// (`parse_graph_type_constraint_spec` enforces this), which tells it apart from the
    /// unnamed constraints a node/edge type specification owns
    /// (`parse_graph_type_owned_constraints`). The error points at `open`,
    /// the body's `{`.
    pub(in crate::parser) fn reject_undesignated_constraint_drops(
        &self,
        open: &Token,
        body: &GraphTypeBody,
    ) -> ParseResult<()> {
        let undesignated = body.constraints.iter().any(|c| {
            c.name.is_none() && !matches!(c.target, GraphTypeElementRef::InSitu { alias: None, .. })
        });
        if undesignated {
            return Err(self.error_at_token(
                open,
                "undesignated constraints cannot be dropped by specification",
            ));
        }
        Ok(())
    }

    /// One comma-separated top-level element of a `<graph type
    /// specification body>`: a `CONSTRAINT` specification, or a node type
    /// specification `(...)` -- the source endpoint reference of an edge
    /// type specification when a `-` follows it. Either type specification
    /// may end in an inline `REQUIRE ...` constraint list (see
    /// `parse_graph_type_owned_constraints`).
    pub(in crate::parser) fn parse_graph_type_element(
        &mut self,
        body: &mut GraphTypeBody,
    ) -> ParseResult<()> {
        if self.peek_type() == TokenType::CONSTRAINT {
            return self.parse_graph_type_constraint_element(body);
        }

        self.expect(TokenType::LeftParen)?;
        let (alias, label, implies) = self.parse_graph_type_ref()?;
        let mut implied_labels = Vec::new();
        if implies && matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
            self.next_token();
            implied_labels.push(self.parse_label_name()?);
            while self.peek_type() == TokenType::Ampersand {
                self.next_token();
                // Each subsequent `&`-joined label may repeat its own
                // `:`/IS introducer (`:Zebra&:Mango`), or omit it
                // (`:Happy&Employed`) -- both spellings denote the same
                // implied label set.
                if matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
                    self.next_token();
                }
                implied_labels.push(self.parse_label_name()?);
            }
        }
        let properties = self.parse_graph_type_property_block(implies, "Node")?;
        self.expect(TokenType::RightParen)?;

        if self.peek_type() == TokenType::Dash {
            // Edge type specification: the just-parsed `(...)` was its
            // source endpoint reference (`implies` is significant here --
            // an implies reference must name a declared node type -- so
            // it's threaded through rather than discarded).
            let source = Self::build_graph_type_ref_implies(alias, label, implies);
            return self.parse_graph_type_edge_tail(body, source);
        }
        let node = GraphTypeNodeTypeSpec {
            alias,
            identifying_label: label.unwrap_or_default(),
            implied_labels,
            properties,
        };
        self.parse_graph_type_node_tail(body, node)
    }

    /// A `CONSTRAINT` element of a `<graph type specification body>`,
    /// starting at `CONSTRAINT`: a CIP-182 bare drop-by-name, else a full
    /// constraint specification.
    pub(in crate::parser) fn parse_graph_type_constraint_element(
        &mut self,
        body: &mut GraphTypeBody,
    ) -> ParseResult<()> {
        // `<constraint drop specification>` ::= `CONSTRAINT
        // <name>` -- a bare name with no `FOR ...`, so the element ends
        // right after the name. It is read in every body; the callers of
        // every verb but `DROP` reject it (`reject_bare_constraint_drops`).
        // Ending the lookahead at `,`/`}` rather than "anything but `FOR`"
        // keeps an unnamed `CONSTRAINT FOR ...` specification from reading
        // `FOR` as the constraint name (a `DROP` caller rejects that
        // specification instead -- `reject_undesignated_constraint_drops`).
        if self.peek_at(1).ty.is_ident_like()
            && matches!(self.peek_at(2).ty, TokenType::Comma | TokenType::RightBrace)
        {
            self.next_token(); // consume CONSTRAINT
            let name = self.parse_bare_name("identifier")?;
            body.drop_constraint_names.push(name);
            return Ok(());
        }
        let c = self.parse_graph_type_constraint_spec()?;
        body.constraints.push(c);
        Ok(())
    }

    /// The rest of a node type specification once its `(...)` is parsed:
    /// the trailing inline `REQUIRE ...` list. Records `node` and every
    /// constraint it owns in `body`.
    pub(in crate::parser) fn parse_graph_type_node_tail(
        &mut self,
        body: &mut GraphTypeBody,
        node: GraphTypeNodeTypeSpec,
    ) -> ParseResult<()> {
        let constraints = self.parse_graph_type_owned_constraints(
            &node.properties,
            &node.identifying_label,
            false,
            node.alias.as_ref(),
        )?;
        body.constraints.extend(constraints);
        body.node_types.push(node);
        Ok(())
    }

    /// The rest of an edge type specification once its `source` endpoint
    /// reference is parsed: `'-' '[' <edge type head> ']' '->' <destination
    /// endpoint>` and the trailing inline `REQUIRE ...` list. Records the
    /// edge type and every constraint it owns in `body`.
    pub(in crate::parser) fn parse_graph_type_edge_tail(
        &mut self,
        body: &mut GraphTypeBody,
        source: GraphTypeElementRef,
    ) -> ParseResult<()> {
        self.next_token(); // consume '-'
        self.expect(TokenType::LeftBracket)?;
        let (alias, label, implies) = self.parse_graph_type_ref()?;
        let identifying_label = label.unwrap_or_default();
        let properties = self.parse_graph_type_property_block(implies, "Edge")?;
        self.expect(TokenType::RightBracket)?;
        self.expect(TokenType::ArrowRight)?;
        self.expect(TokenType::LeftParen)?;
        let (t_alias, t_label, t_implies) = self.parse_graph_type_ref()?;
        self.expect(TokenType::RightParen)?;
        let target = Self::build_graph_type_ref_implies(t_alias, t_label, t_implies);

        let constraints = self.parse_graph_type_owned_constraints(
            &properties,
            &identifying_label,
            true,
            alias.as_ref(),
        )?;
        body.constraints.extend(constraints);
        body.edge_types.push(GraphTypeEdgeTypeSpec {
            alias,
            identifying_label,
            properties,
            source,
            target,
        });
        Ok(())
    }

    /// The constraints a node/edge type specification owns, all targeting
    /// its own identifying label (CIP-178 Rules 10-12): one per property
    /// carrying an inline constraint, then one per trailing inline
    /// `REQUIRE ...`. `owner_alias` is the specification's own alias.
    pub(in crate::parser) fn parse_graph_type_owned_constraints(
        &mut self,
        properties: &[GraphTypePropertyType],
        identifying_label: &SmallName,
        is_edge: bool,
        owner_alias: Option<&SmallName>,
    ) -> ParseResult<Vec<GraphTypeConstraintSpec>> {
        let mut constraints =
            Self::take_property_inline_constraints(properties, identifying_label, is_edge);
        let own_ref = GraphTypeElementRef::InSitu {
            alias: None,
            label: identifying_label.clone(),
            implies: true,
        };
        while self.peek_type() == TokenType::REQUIRE {
            constraints.push(self.parse_graph_type_inline_require(
                own_ref.clone(),
                is_edge,
                owner_alias,
            )?);
        }
        Ok(constraints)
    }

    /// `[alias] [(':'|IS) <label>]` -- the alias/label pair shared by every
    /// `<node type reference>`/`<edge type reference>`/`<node type
    /// specification>`/`<edge type specification>` head. `IS` is a
    /// reserved token (never itself a legal alias in this grammar), so
    /// checking for it before the ident-like alias check disambiguates
    /// `(p IS Person => ...)` (alias `p`) from a hypothetical alias-less
    /// `IS`-introduced label. The label may be a keyword (ADR-0169); the
    /// alias may not.
    pub(in crate::parser) fn parse_graph_type_ref_inner(
        &mut self,
    ) -> ParseResult<(Option<SmallName>, Option<SmallName>)> {
        let mut alias = None;
        if self.peek_type().is_ident_like() && self.peek_type() != TokenType::IS {
            alias = Some(self.parse_bare_name("identifier")?);
        }
        let mut label = None;
        if self.peek_type() == TokenType::Colon || self.peek_type() == TokenType::IS {
            self.next_token();
            label = Some(self.parse_label_name()?);
        }
        Ok((alias, label))
    }

    /// Consumes a `=>` or `IMPLIES` token if present; returns whether one
    /// was found.
    pub(in crate::parser) fn consume_graph_type_implies(&mut self) -> bool {
        if self.peek_type() == TokenType::FatArrow || self.peek_is_ident("implies") {
            self.next_token();
            true
        } else {
            false
        }
    }

    /// `'{' [ <property type> (',' <property type>)* ] '}'` (CIP-178
    /// `<property types specification>`). `<property type> ::= <name>
    /// [('::'|TYPED)] <value type> [DEFAULT ON COMMIT <value expression>]
    /// [<inline constraint>]` (with the `DEFAULT ON COMMIT` clause;
    /// grammar-only here, the shape and the preview gate live in the
    /// semantic layer and statement admission).
    pub(in crate::parser) fn parse_graph_type_property_types(
        &mut self,
    ) -> ParseResult<Vec<GraphTypePropertyType>> {
        self.expect(TokenType::LeftBrace)?;
        let mut props = Vec::new();
        if self.peek_type() != TokenType::RightBrace {
            loop {
                let name = self.parse_bare_name("identifier")?;
                // ADR-0211: `<name> IS [::] <value type>` is the GQL
                // spelling of `<name> :: <value type>`. The optional `IS`
                // introducer is consumed once, guarded by the inline-
                // constraint lookahead (`IS KEY`/`IS UNIQUE` belong to the
                // constraint parsed below); the `::`/TYPED introducer then
                // has a single branch.
                if self.peek_type() == TokenType::IS
                    && !self.peek_starts_graph_type_inline_constraint()
                {
                    self.next_token(); // consume IS
                }
                if self.peek_type() == TokenType::DoubleColon || self.peek_is_typed_keyword() {
                    self.next_token();
                }
                let value_type = self.parse_value_type()?;
                let default_value = self.parse_graph_type_property_default()?;
                let inline_constraint = if self.peek_type() == TokenType::IS {
                    Some(self.parse_graph_type_inline_property_constraint()?)
                } else {
                    None
                };
                props.push(GraphTypePropertyType {
                    name,
                    value_type,
                    default_value,
                    inline_constraint,
                });
                if self.peek_type() != TokenType::Comma {
                    break;
                }
                self.next_token();
            }
        }
        self.expect(TokenType::RightBrace)?;
        Ok(props)
    }

    /// The optional `DEFAULT ON COMMIT <value expression>` clause of a
    /// graph-type `<property type>` (CIP-262): `DEFAULT` and `COMMIT` are
    /// non-reserved words, so both match as bare identifiers; `ON` is the
    /// keyword token. The value is the ordinary expression parser's -- its
    /// existing nesting-depth limit (ADR-0079) covers the clause, so no
    /// new recursion family is introduced. Shape (literal or type
    /// constructor, never `null`) is the semantic layer's to enforce.
    pub(super) fn parse_graph_type_property_default(&mut self) -> ParseResult<Option<Expr>> {
        if !self.peek_is_ident("default") {
            return Ok(None);
        }
        self.next_token(); // consume DEFAULT
        self.expect(TokenType::ON)?;
        if !self.peek_is_ident("commit") {
            return Err(self.error_at_token(
                self.peek(),
                "expected COMMIT after DEFAULT ON in a property type".to_string(),
            ));
        }
        self.next_token(); // consume COMMIT
        Ok(Some(self.parse_expression()?))
    }

    /// The `{ <property types> }` block of a node/edge type specification.
    /// The `=>`/`IMPLIES` marker (the `implies` flag, consumed by the
    /// caller) is what makes the preceding label the *explicitly denoted
    /// identifying label*, so a bare `{` without it is a compile-time
    /// syntax error (CIP-178/181/183; current Neo4j rejects the bare
    /// `(:Label {…})` form); no block at all is an empty property list.
    /// One policy for both the node and the edge type-spec parsers.
    pub(super) fn parse_graph_type_property_block(
        &mut self,
        implies: bool,
        entity: &str,
    ) -> ParseResult<Vec<GraphTypePropertyType>> {
        if self.peek_type() != TokenType::LeftBrace {
            return Ok(Vec::new());
        }
        if !implies {
            return Err(self.error_at_token(
                self.peek(),
                format!(
                    "{entity} type does not have an explicitly denoted identifying label \
                     (use `=>` or `IMPLIES` after the label before the property block)"
                ),
            ));
        }
        self.parse_graph_type_property_types()
    }

    /// A KEY/UNIQUE token mapped to its inline-constraint verb (UNIQUE is a
    /// keyword token, KEY a non-reserved identifier) — the single definition
    /// of the verb token set both the inline-constraint lookahead and its
    /// parser use.
    pub(super) fn inline_verb_from_token(tok: &Token) -> Option<GraphTypeInlineVerb> {
        match tok.ty {
            TokenType::UNIQUE => Some(GraphTypeInlineVerb::Unique),
            TokenType::Identifier if Self::token_is_ident(tok, "key") => {
                Some(GraphTypeInlineVerb::Key)
            }
            _ => None,
        }
    }

    /// An entity qualifier token (NODE / REL / identifier `relationship`)
    /// mapped to its qualifier — the single definition of the qualifier
    /// token set both the lookahead and the parser use.
    pub(super) fn entity_qualifier_from_token(tok: &Token) -> Option<GraphTypeEntityQualifier> {
        match tok.ty {
            TokenType::NODE => Some(GraphTypeEntityQualifier::Node),
            TokenType::REL => Some(GraphTypeEntityQualifier::Rel),
            TokenType::Identifier if Self::token_is_ident(tok, "relationship") => {
                Some(GraphTypeEntityQualifier::Rel)
            }
            _ => None,
        }
    }

    /// Whether the tokens after a property declaration's `IS` (not yet
    /// consumed) start an inline constraint -- `IS KEY`, `IS UNIQUE`, or
    /// `IS NODE/REL/RELATIONSHIP KEY|UNIQUE` -- rather than an ADR-0211
    /// `IS [::] <value type>` type declaration.
    pub(in crate::parser) fn peek_starts_graph_type_inline_constraint(&self) -> bool {
        if Self::inline_verb_from_token(self.peek_at(1)).is_some() {
            return true;
        }
        Self::entity_qualifier_from_token(self.peek_at(1)).is_some()
            && Self::inline_verb_from_token(self.peek_at(2)).is_some()
    }

    /// `IS [NODE|REL|RELATIONSHIP] (KEY|UNIQUE)` -- an inline property-type
    /// constraint.
    pub(in crate::parser) fn parse_graph_type_inline_property_constraint(
        &mut self,
    ) -> ParseResult<GraphTypeInlineConstraint> {
        self.expect(TokenType::IS)?;
        let qualifier = Self::entity_qualifier_from_token(self.peek());
        if qualifier.is_some() {
            self.next_token();
        }
        let tok = self.next_token();
        match Self::inline_verb_from_token(&tok) {
            Some(verb) => Ok(GraphTypeInlineConstraint { verb, qualifier }),
            None => Err(self.error_at_token(
                &tok,
                format!("expected KEY or UNIQUE, got {:?}", tok.literal),
            )),
        }
    }

    /// Rewrites every property in `properties` carrying an inline
    /// constraint into a standalone `GraphTypeConstraintSpec` targeting the
    /// owning node/edge type's own identifying label (CIP-178 Rules 10,
    /// 12).
    pub(in crate::parser) fn take_property_inline_constraints(
        properties: &[GraphTypePropertyType],
        identifying_label: &SmallName,
        is_edge: bool,
    ) -> Vec<GraphTypeConstraintSpec> {
        properties
            .iter()
            .filter_map(|p| {
                p.inline_constraint.map(|ic| GraphTypeConstraintSpec {
                    name: None,
                    target: GraphTypeElementRef::InSitu {
                        alias: None,
                        label: identifying_label.clone(),
                        implies: true,
                    },
                    is_edge,
                    properties: vec![p.name.clone()],
                    kind: match ic.verb {
                        GraphTypeInlineVerb::Key => GraphTypeConstraintVerb::Key,
                        GraphTypeInlineVerb::Unique => GraphTypeConstraintVerb::Unique,
                    },
                    property_type: None,
                    options: OptionsForm::Absent,
                })
            })
            .collect()
    }

    /// A node/edge type specification's trailing inline `REQUIRE
    /// <property name spec> <verb> [OPTIONS <map>]` (no `CONSTRAINT`
    /// keyword, no name -- CIP-178 Rules 11/12). `target` is always the
    /// owning element's own identifying label, in-situ-identified form (see
    /// `parse_graph_type_owned_constraints`). `owner_alias` is the owning
    /// node/edge type specification's own declared element type alias, if
    /// any -- every alias used in the REQUIRE list must match it exactly.
    pub(in crate::parser) fn parse_graph_type_inline_require(
        &mut self,
        target: GraphTypeElementRef,
        is_edge: bool,
        owner_alias: Option<&SmallName>,
    ) -> ParseResult<GraphTypeConstraintSpec> {
        self.next_token(); // consume REQUIRE
        let properties =
            self.parse_graph_type_property_name_spec_for_owner(owner_alias, is_edge)?;
        let GraphTypeConstraintVerbSpec {
            kind,
            property_type,
            options,
        } = self.parse_graph_type_constraint_verb()?;
        // In the grammar, `<node/edge type inline constraint require>` is
        // only ever a KEY or UNIQUE constraint require -- existence
        // (`IS NOT NULL`) and property type (`:: T`) constraints are not
        // grammatically reachable when attached directly to a node/edge
        // type specification's trailing `REQUIRE` (only via the standalone
        // `CONSTRAINT FOR` form or an inline property constraint).
        if matches!(
            kind,
            GraphTypeConstraintVerb::Exists | GraphTypeConstraintVerb::PropertyType
        ) {
            let kind_word = if is_edge { "edge" } else { "node" };
            let verb_word = if matches!(kind, GraphTypeConstraintVerb::Exists) {
                "existence"
            } else {
                "type"
            };
            return Err(self.error(format!(
                "Unsupported syntax — {kind_word} type property {verb_word} constraints cannot be specified inline of a {kind_word} type"
            )));
        }
        Ok(GraphTypeConstraintSpec {
            name: None,
            target,
            is_edge,
            properties,
            kind,
            property_type,
            options,
        })
    }

    /// `CONSTRAINT [<name>] FOR <node/edge type reference> REQUIRE
    /// <property name spec> <verb> [OPTIONS <map>]` (CIP-178 `<constraint
    /// specification>`). Enforces the purely-local rules that don't need
    /// cross-element context: the any type is forbidden, a labelled
    /// reference requires an element type alias, the property name spec's
    /// own alias must match the reference's, no duplicate property name in
    /// the spec, and the "existence/property-type constraints are not
    /// allowed on a structurally-identified reference" rule. Rules needing
    /// the *whole* body (5-9, 14, 15/16, reference resolution) are checked
    /// by the engine's graph-type validator.
    pub(in crate::parser) fn parse_graph_type_constraint_spec(
        &mut self,
    ) -> ParseResult<GraphTypeConstraintSpec> {
        self.expect(TokenType::CONSTRAINT)?;
        let name = if self.peek_type() == TokenType::FOR {
            None
        } else {
            Some(self.parse_bare_name("identifier")?)
        };
        self.expect(TokenType::FOR)?;
        let (is_edge, target) = self.parse_graph_type_constraint_target()?;

        let expected_alias = match &target {
            GraphTypeElementRef::Any => {
                return Err(self.error("constraint may not reference the any node type"));
            }
            GraphTypeElementRef::AliasRef(a) => Some(a.clone()),
            GraphTypeElementRef::InSitu { alias: Some(a), .. } => Some(a.clone()),
            GraphTypeElementRef::InSitu { alias: None, .. } => {
                return Err(self.error(if is_edge {
                    "edge type constraint reference requires an element type alias"
                } else {
                    "node type constraint reference requires an element type alias"
                }));
            }
        };

        self.expect(TokenType::REQUIRE)?;
        let properties = self.parse_graph_type_property_name_spec(expected_alias.as_ref())?;
        let GraphTypeConstraintVerbSpec {
            kind,
            property_type,
            options,
        } = self.parse_graph_type_constraint_verb()?;

        // Rules 22/23: existence/property-type constraints are not allowed
        // on a structurally-identified reference (an alias reference always
        // resolves to a declared type; an in-situ reference with `=>` must
        // resolve to one).
        let locally_identified = matches!(target, GraphTypeElementRef::AliasRef(_))
            || matches!(target, GraphTypeElementRef::InSitu { implies: true, .. });
        if locally_identified
            && matches!(
                kind,
                GraphTypeConstraintVerb::Exists | GraphTypeConstraintVerb::PropertyType
            )
        {
            return Err(self.error(
                "existence/property type constraints are not allowed on a designated node type",
            ));
        }

        Ok(GraphTypeConstraintSpec {
            name,
            target,
            is_edge,
            properties,
            kind,
            property_type,
            options,
        })
    }

    /// Parses a `<constraint specification>`'s `FOR` target: `'(' ...  ')'`
    /// for a node type reference, or `'(' ')' '-' '[' ... ']' '->' '(' ')'`
    /// for an edge type reference (both node-parens empty). Returns
    /// `(is_edge, reference)`.
    pub(in crate::parser) fn parse_graph_type_constraint_target(
        &mut self,
    ) -> ParseResult<(bool, GraphTypeElementRef)> {
        self.expect(TokenType::LeftParen)?;
        if self.peek_type() == TokenType::RightParen && self.peek_at(1).ty == TokenType::Dash {
            self.next_token(); // consume ')'
            self.expect(TokenType::Dash)?;
            self.expect(TokenType::LeftBracket)?;
            let (alias, label, implies) = self.parse_graph_type_ref()?;
            self.expect(TokenType::RightBracket)?;
            self.expect(TokenType::ArrowRight)?;
            self.expect(TokenType::LeftParen)?;
            self.expect(TokenType::RightParen)?;
            return Ok((
                true,
                Self::build_graph_type_ref_implies(alias, label, implies),
            ));
        }
        let (alias, label, implies) = self.parse_graph_type_ref()?;
        self.expect(TokenType::RightParen)?;
        Ok((
            false,
            Self::build_graph_type_ref_implies(alias, label, implies),
        ))
    }

    /// A `<graph type reference>` head plus its `=>`/`IMPLIES` marker: the
    /// alias/label pair of [`Parser::parse_graph_type_ref_inner`] with the
    /// implies marker consumed behind it — the marker only counts, and is
    /// only consumed, behind a label; `[: => {...}]` has no identifying
    /// label to denote. The five reference positions (a node/edge type
    /// specification's own head, an edge's destination endpoint, and the
    /// two shapes of a constraint `FOR`-target) all spell exactly this.
    pub(in crate::parser) fn parse_graph_type_ref(
        &mut self,
    ) -> ParseResult<(Option<SmallName>, Option<SmallName>, bool)> {
        let (alias, label) = self.parse_graph_type_ref_inner()?;
        let implies = label.is_some() && self.consume_graph_type_implies();
        Ok((alias, label, implies))
    }

    /// Builds a `GraphTypeElementRef` from a parsed alias/label/implies
    /// triple -- shared by node/edge type spec endpoints and constraint
    /// `FOR`-targets, where `=>` presence is semantically meaningful
    /// (an implies reference must name a declared identifying label;
    /// existence/property-type constraints are not allowed on a
    /// structurally-identified reference). CIP-178's edge-endpoint rule
    /// (an edge type's own in-situ
    /// endpoint reference may not carry an alias) and the constraint
    /// `FOR`-target rules (the any type is forbidden, a labelled
    /// reference requires an alias) are cross-referential/context-dependent
    /// and are checked by the engine's graph-type validator, not here.
    pub(in crate::parser) fn build_graph_type_ref_implies(
        alias: Option<SmallName>,
        label: Option<SmallName>,
        implies: bool,
    ) -> GraphTypeElementRef {
        match (alias, label) {
            (None, None) => GraphTypeElementRef::Any,
            (Some(a), None) => GraphTypeElementRef::AliasRef(a),
            (alias, Some(l)) => GraphTypeElementRef::InSitu {
                alias,
                label: l,
                implies,
            },
        }
    }

    /// `<alias>.<prop>` / `'(' <alias>.<prop> (',' <alias>.<prop>)* ')'`
    /// (`<property name specification>`), returning the raw
    /// `(alias, property)` pairs in source order without any alias
    /// validation -- shared by `parse_graph_type_property_name_spec` (the
    /// standalone `CONSTRAINT FOR` form, which already knows a single
    /// `expected_alias`) and `parse_graph_type_inline_require`'s owner-alias
    /// check (which needs to report every *distinct* offending alias, not
    /// just detect a mismatch).
    pub(in crate::parser) fn parse_graph_type_property_name_entries(
        &mut self,
    ) -> ParseResult<Vec<(SmallName, SmallName)>> {
        let mut entries: Vec<(SmallName, SmallName)> = Vec::new();
        if self.peek_type() == TokenType::LeftParen {
            self.next_token();
            loop {
                let alias = self.parse_bare_name("identifier")?;
                self.expect(TokenType::Dot)?;
                let prop = self.parse_bare_name("identifier")?;
                entries.push((alias, prop));
                if self.peek_type() != TokenType::Comma {
                    break;
                }
                self.next_token();
            }
            self.expect(TokenType::RightParen)?;
        } else {
            let alias = self.parse_bare_name("identifier")?;
            self.expect(TokenType::Dot)?;
            let prop = self.parse_bare_name("identifier")?;
            entries.push((alias, prop));
        }
        Ok(entries)
    }

    /// `<alias>.<prop>` / `'(' <alias>.<prop> (',' <alias>.<prop>)* ')'`
    /// (`<property name specification>`). When `expected_alias` is
    /// given, every entry's alias must match it; duplicate
    /// property names within the spec are always rejected.
    pub(in crate::parser) fn parse_graph_type_property_name_spec(
        &mut self,
        expected_alias: Option<&SmallName>,
    ) -> ParseResult<Vec<SmallName>> {
        let entries = self.parse_graph_type_property_name_entries()?;
        if let Some(expected) = expected_alias {
            if entries.iter().any(|(a, _)| a != expected) {
                return Err(self.error("property name alias must match the reference alias"));
            }
        }
        let mut props = Vec::new();
        for (_, p) in entries {
            if props.contains(&p) {
                return Err(self.error("duplicate property name in property names specification"));
            }
            props.push(p);
        }
        Ok(props)
    }

    /// The `<property names specification>` of a node/edge type
    /// specification's trailing inline `REQUIRE` list (CIP-178 `<node/edge
    /// type inline constraint require>`). Unlike the standalone `CONSTRAINT
    /// FOR` form (`parse_graph_type_property_name_spec`, which always has a
    /// declared reference alias to compare against), the owning node/edge
    /// type specification may not have declared an alias at all
    /// (`owner_alias: None`) -- in which case *any* alias used in the
    /// REQUIRE list is undefined, not just a non-matching one.
    pub(in crate::parser) fn parse_graph_type_property_name_spec_for_owner(
        &mut self,
        owner_alias: Option<&SmallName>,
        is_edge: bool,
    ) -> ParseResult<Vec<SmallName>> {
        let entries = self.parse_graph_type_property_name_entries()?;
        let mut undefined: Vec<SmallName> = Vec::new();
        for (a, _) in &entries {
            if Some(a) != owner_alias && !undefined.contains(a) {
                undefined.push(a.clone());
            }
        }
        if !undefined.is_empty() {
            let kind = if is_edge { "Edge" } else { "Node" };
            let (noun, list) = if undefined.len() == 1 {
                ("alias", undefined[0].to_string())
            } else {
                (
                    "aliases",
                    undefined
                        .iter()
                        .map(SmallName::to_string)
                        .collect::<Vec<_>>()
                        .join(" and "),
                )
            };
            return Err(self.error(format!(
                "{kind} type constraint uses undefined type {noun} {list}"
            )));
        }
        let mut props = Vec::new();
        for (_, p) in entries {
            if props.contains(&p) {
                return Err(self.error("duplicate property name in property names specification"));
            }
            props.push(p);
        }
        Ok(props)
    }

    /// The constraint verb tail of a `<constraint specification>` (CIP-178
    /// CS grammar), starting right after the `<property name spec>`:
    /// either a bare `:: <value type>` or `IS (KEY | [NODE|REL|
    /// RELATIONSHIP] KEY | UNIQUE | [NODE|REL|RELATIONSHIP] UNIQUE | NOT
    /// NULL | (::|TYPED) <value type>)`, followed by an optional `OPTIONS
    /// <map>`. Mirrors (but does not share code with) the top-level
    /// `CREATE CONSTRAINT` verb grammar in `parse_create_constraint` --
    /// unlike that grammar, the entity qualifier here is never restricted
    /// by context (CIP-178 doesn't test a mismatch).
    pub(in crate::parser) fn parse_graph_type_constraint_verb(
        &mut self,
    ) -> ParseResult<GraphTypeConstraintVerbSpec> {
        if self.peek_type() == TokenType::DoubleColon {
            self.next_token();
            let te = self.parse_value_type()?;
            let options = self.parse_options_tail()?;
            return Ok(GraphTypeConstraintVerbSpec {
                kind: GraphTypeConstraintVerb::PropertyType,
                property_type: Some(te),
                options,
            });
        }
        self.expect(TokenType::IS)?;
        let tok = self.peek().clone();
        let (verb, property_type) = match tok.ty {
            TokenType::UNIQUE => {
                self.next_token();
                (GraphTypeConstraintVerb::Unique, None)
            }
            TokenType::NOT => {
                self.next_token();
                self.expect(TokenType::NULL)?;
                (GraphTypeConstraintVerb::Exists, None)
            }
            TokenType::NODE | TokenType::REL => {
                self.next_token();
                self.parse_graph_type_constraint_kw_verb()?
            }
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("relationship") => {
                self.next_token();
                self.parse_graph_type_constraint_kw_verb()?
            }
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("key") => {
                self.next_token();
                (GraphTypeConstraintVerb::Key, None)
            }
            TokenType::DoubleColon => {
                self.next_token();
                let te = self.parse_value_type()?;
                (GraphTypeConstraintVerb::PropertyType, Some(te))
            }
            _ if self.peek_is_typed_keyword() => {
                self.next_token();
                let te = self.parse_value_type()?;
                (GraphTypeConstraintVerb::PropertyType, Some(te))
            }
            _ => {
                return Err(self.error_at_token(
                    &tok,
                    format!(
                        "expected UNIQUE, NOT NULL, KEY, ::, or TYPED, got {:?}",
                        tok.literal
                    ),
                ));
            }
        };
        let options = self.parse_options_tail()?;
        Ok(GraphTypeConstraintVerbSpec {
            kind: verb,
            property_type,
            options,
        })
    }

    pub(in crate::parser) fn parse_graph_type_constraint_kw_verb(
        &mut self,
    ) -> ParseResult<(GraphTypeConstraintVerb, Option<TypeExpr>)> {
        let tok = self.next_token();
        if tok.ty == TokenType::UNIQUE {
            Ok((GraphTypeConstraintVerb::Unique, None))
        } else if tok.literal.eq_ignore_ascii_case("key") {
            Ok((GraphTypeConstraintVerb::Key, None))
        } else {
            Err(self.error_at_token(
                &tok,
                format!("expected UNIQUE or KEY, got {:?}", tok.literal),
            ))
        }
    }
}
