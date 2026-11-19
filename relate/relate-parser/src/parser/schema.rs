use super::*;

/// What the `ON` target of a `CREATE … INDEX … FOR …` clause must be,
/// decided once from the index kind, the entity type and whether `EACH` was
/// written ([`Parser::classify_on_target`]).
enum OnTargetForm {
    /// `ON EACH [prop, …]` — FULLTEXT's list.
    EachBracketList,
    /// `ON EACH labels(n)` / `ON EACH type(r)`, or a relationship LOOKUP
    /// index's EACH-less `ON type(r)`.
    LookupTarget,
    /// `ON (n.prop, …)` / `ON n.prop` — the property-list kinds.
    PropertyList,
}

impl<'a> Parser<'a> {
    /// Parses the `FOR (n:Label)` / `FOR (n:A|B|...)` / `FOR (n)` /
    /// `FOR ()-[r:TYPE]-()` (all four direction spellings) / `FOR ()-[r]-()`
    /// DDL target shared by `CREATE INDEX` and `CREATE CONSTRAINT`
    /// (CIP-11's relationship-pattern generalization). Returns the entity
    /// kind, the flat label/relationship-type list (empty for a label-less
    /// LOOKUP target), and the pattern's own variable name (the property
    /// list that follows, e.g. `ON (n.prop)`, is written in terms of it).
    pub(super) fn parse_schema_for_target(
        &mut self,
    ) -> ParseResult<(SchemaEntityType, Vec<SmallName>, SmallName)> {
        self.expect(TokenType::FOR)?;
        if self.peek_type() != TokenType::LeftParen {
            let tok = self.peek().clone();
            return Err(self.error_at_token(
                &tok,
                format!("expected '(' after FOR, got {:?}", tok.literal),
            ));
        }
        // A relationship pattern always has empty parens on both sides in
        // this DDL grammar (`FOR ()-[r:TYPE]-()`, `FOR ()<-[r:TYPE]-()`, ...)
        // -- distinguished from the node form by a `)` immediately followed
        // by a dash/left-arrow. Unlike `parse_constraint_for_target_ext`
        // (a more permissive `FOR (m)-[...]->()` grammar), this stays
        // strict: `CREATE INDEX`/`CREATE VECTOR INDEX` reject a
        // labelled or aliased endpoint on either side (CIP-116 create-
        // index-errors[18]/[19]).
        if self.peek_at(1).ty == TokenType::RightParen && self.peek_at(2).ty.is_relationship_start()
        {
            self.parse_node_pattern()?; // consume the empty left `()`
            let rel = self.parse_rel_pattern()?;
            self.parse_node_pattern()?; // consume the (also empty) right `()`
            let labels = match &rel.type_expr {
                Some(le) => le.as_ddl_list().ok_or_else(|| {
                    self.error(
                        "expected a plain relationship type or A|B|... list in the FOR pattern",
                    )
                })?,
                None => Vec::new(),
            };
            let var = rel.variable.clone().unwrap_or_default();
            return Ok((SchemaEntityType::Relationship, labels, var));
        }

        let node = self.parse_node_pattern()?;
        let labels = match &node.label_expr {
            Some(le) => le.as_ddl_list().ok_or_else(|| {
                self.error("expected a plain label or A|B|... list in the FOR pattern")
            })?,
            None => Vec::new(),
        };
        let var = node.variable.clone().unwrap_or_default();
        Ok((SchemaEntityType::Node, labels, var))
    }

    /// Like `parse_schema_for_target`, but for `CREATE CONSTRAINT` only:
    /// also returns the relationship pattern's own source/destination
    /// endpoint aliases, and permits a labelled/aliased endpoint on either
    /// side (CIP-177's `FOR (m)-[:TYPE]->()` / `FOR ()-[:TYPE]->(m)`
    /// label-existence constraints need to know which endpoint `m`
    /// names). `None`/`None` for a node target. Deliberately not shared
    /// with `parse_schema_for_target` -- `CREATE INDEX` must keep
    /// rejecting a labelled/aliased relationship-pattern endpoint (CIP-116
    /// create-index-errors[18]/[19]).
    pub(super) fn parse_constraint_for_target_ext(&mut self) -> ParseResult<ConstraintForTarget> {
        self.expect(TokenType::FOR)?;
        if self.peek_type() != TokenType::LeftParen {
            let tok = self.peek().clone();
            return Err(self.error_at_token(
                &tok,
                format!("expected '(' after FOR, got {:?}", tok.literal),
            ));
        }
        // Try the source endpoint as a general node pattern (empty `()`,
        // or one carrying just an alias like `(m)` -- CIP-177's
        // source/destination label-existence constraints) -- distinguished
        // from a plain node FOR-target by the closing `)` being immediately
        // followed by a dash/left-arrow.
        let saved = self.save_pos();
        let source = self.parse_node_pattern()?;
        if self.peek_type().is_relationship_start() {
            let rel = self.parse_rel_pattern()?;
            let target = self.parse_node_pattern()?;
            let labels = match &rel.type_expr {
                Some(le) => le.as_ddl_list().ok_or_else(|| {
                    self.error(
                        "expected a plain relationship type or A|B|... list in the FOR pattern",
                    )
                })?,
                None => Vec::new(),
            };
            return Ok(ConstraintForTarget {
                entity_type: SchemaEntityType::Relationship,
                labels,
                variable: rel.variable.unwrap_or_default(),
                source_alias: source.variable,
                target_alias: target.variable,
            });
        }
        self.restore_pos(saved);

        let node = self.parse_node_pattern()?;
        let labels = match &node.label_expr {
            Some(le) => le.as_ddl_list().ok_or_else(|| {
                self.error("expected a plain label or A|B|... list in the FOR pattern")
            })?,
            None => Vec::new(),
        };
        Ok(ConstraintForTarget {
            entity_type: SchemaEntityType::Node,
            labels,
            variable: node.variable.unwrap_or_default(),
            source_alias: None,
            target_alias: None,
        })
    }

    /// `ON (n.prop1, n.prop2, ...)` / paren-less single-property `ON
    /// n.prop`.
    pub(super) fn parse_on_property_list(&mut self) -> ParseResult<Vec<SmallName>> {
        let paren = self.peek_type() == TokenType::LeftParen;
        if paren {
            self.next_token();
        }
        let mut properties = Vec::new();
        loop {
            self.next_token(); // variable name (skip)
            self.expect(TokenType::Dot)?;
            properties.push(self.parse_bare_name("property name")?);
            if !paren || self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        if paren {
            self.expect(TokenType::RightParen)?;
        }
        Ok(properties)
    }

    /// `ON EACH [n.prop1, n.prop2, ...]` (CIP-38 FULLTEXT): a bracketed list
    /// of plain property references -- Neo4j's actual grammar is narrower
    /// than the proposal's own `EACH <expression>` text (Deviation: no general
    /// expressions, e.g. `p.title + p.abstract`, are accepted), and the
    /// list may never be empty.
    pub(super) fn parse_on_each_bracket_list(&mut self) -> ParseResult<Vec<SmallName>> {
        self.expect(TokenType::LeftBracket)?;
        if self.peek_type() == TokenType::RightBracket {
            return Err(self.error("InvalidSyntax: ON EACH requires at least one property"));
        }
        let mut properties = Vec::new();
        loop {
            self.next_token(); // variable name (skip)
            self.expect(TokenType::Dot)?;
            properties.push(self.parse_bare_name("property name")?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightBracket)?;
        let mut seen = std::collections::HashSet::new();
        for p in &properties {
            if !seen.insert(p.clone()) {
                return Err(self.error(format!(
                    "IndexWithDuplicatedTokens: duplicate property '{p}' in ON EACH list"
                )));
            }
        }
        Ok(properties)
    }

    /// `ON EACH labels(n)` / `ON EACH type(r)`: the LOOKUP token-index
    /// target. The argument is the FOR-pattern's own
    /// variable; a LOOKUP index has no fixed property/label list, so
    /// nothing beyond syntax is captured here.
    pub(super) fn parse_lookup_each_target(
        &mut self,
        entity_type: SchemaEntityType,
        pattern_variable: &SmallName,
    ) -> ParseResult<()> {
        let fn_tok = self.next_token();
        let want = match entity_type {
            SchemaEntityType::Node => "labels",
            SchemaEntityType::Relationship => "type",
        };
        if !fn_tok.literal.eq_ignore_ascii_case(want) {
            return Err(self.error_at_token(
                &fn_tok,
                format!("expected {}(...), got {:?}", want, fn_tok.literal),
            ));
        }
        self.expect(TokenType::LeftParen)?;
        let var_tok = self.next_token();
        // The function argument must be the lookup target's own pattern
        // variable (`FOR (n) ... labels(n)`); any other name is undefined
        // in this scope (Neo4j 42N62). The detail tag names that condition,
        // so `EngineError::kind()` surfaces `UndefinedVariable` like every
        // other undefined-variable site rather than the generic parse
        // fallback.
        if pattern_variable.with_str(|expected| var_tok.literal != expected) {
            return Err(self
                .error_at_token(
                    &var_tok,
                    format!("Variable `{}` not defined.", var_tok.literal),
                )
                .with_detail(crate::parser::UNDEFINED_VARIABLE_DETAIL));
        }
        self.expect(TokenType::RightParen)?;
        Ok(())
    }
}

/// The name-slot continuation tokens of the commands that share the
/// reader below -- the keyword starters that may follow the name instead
/// of naming the object. One closed set, derived from those commands'
/// grammars; a keyword outside it can never continue a name slot, so it
/// is a name -- Neo4j 2026.10's `commandNameExpression` admits every
/// keyword unquoted. Each entry is the token with the command whose name
/// slot it continues -- the one home of both facts, the predicate's set
/// and the tests' per-command pins. Two starters are deliberately absent
/// and stay names (Neo4j-true): `REMOVE`, whose recognized-unsupported
/// ALTER DATABASE arm only ever follows a name, and `RETURN`, which at a
/// SHOW DATABASE name slot names a database -- composing a SHOW without
/// `YIELD` fails the CIP-242 composability rule anyway.
pub(super) const COMMAND_NAME_CONTINUATIONS: [(TokenType, &str); 9] = [
    (
        TokenType::IF,
        "the IF [NOT] EXISTS qualifier of CREATE/DROP INDEX/CONSTRAINT and ALTER DATABASE",
    ),
    (
        TokenType::ON,
        "the legacy ON :Label(prop) target of CREATE/DROP INDEX",
    ),
    (TokenType::FOR, "the CREATE INDEX/CONSTRAINT target"),
    (
        TokenType::SET,
        "ALTER DATABASE's action after the required name",
    ),
    (TokenType::YIELD, "SHOW DATABASE's projection"),
    (TokenType::ORDER, "SHOW DATABASE's ORDER BY paging stage"),
    (TokenType::SKIP, "SHOW DATABASE's SKIP paging stage"),
    (TokenType::LIMIT, "SHOW DATABASE's LIMIT paging stage"),
    (TokenType::WHERE, "SHOW DATABASE's WHERE filter tail"),
];

/// Returns true if the token can spell the `<name>` of a
/// `CREATE INDEX`/`CREATE CONSTRAINT`/`DROP INDEX`/`DROP CONSTRAINT`,
/// `ALTER DATABASE` or `SHOW DATABASE` command: any identifier-or-keyword
/// spelling ([`TokenType::is_schema_name`]) except the
/// [`COMMAND_NAME_CONTINUATIONS`] -- the same layered derivation
/// `TokenType::is_ident_like` takes for variables (LABS-1614: Neo4j
/// 2026.10's `commandNameExpression` admits every keyword unquoted and no
/// string literal, and the continuations are exactly the commands' own
/// tail starters). The one home of that set:
/// `parse_optional_schema_name` routes its match through this predicate,
/// and `parse_required_schema_name` -- the DROP INDEX/CONSTRAINT and
/// ALTER DATABASE name reader -- delegates to that function. The
/// predicate is only the keyword/identifier half of the slot: the
/// reader's own `Parameter` arm admits the `$param` reference (CIP-134).
pub(super) fn is_command_name_token(ty: TokenType) -> bool {
    ty.is_schema_name() && !COMMAND_NAME_CONTINUATIONS.iter().any(|&(t, _)| t == ty)
}

impl<'a> Parser<'a> {
    /// The optional `<name>` in `CREATE INDEX`/`CREATE CONSTRAINT` (and the
    /// `SHOW DATABASE` name slot): an [`is_command_name_token`] spelling —
    /// the derivation directly above, `is_schema_name()` minus the
    /// [`COMMAND_NAME_CONTINUATIONS`], is the one home of the admissible
    /// set and carries the per-command justification (LABS-1614) — or a
    /// `$param` reference (CIP-134). Absent (`None`)
    /// whenever the next token is neither, matching the un-named form.
    pub(super) fn parse_optional_schema_name(&mut self) -> Option<SchemaName> {
        match self.peek_type() {
            TokenType::Parameter => {
                let tok = self.next_token();
                Some(SchemaName::Parameter(tok.literal.into_owned()))
            }
            ty if is_command_name_token(ty) => {
                let tok = self.next_token();
                Some(SchemaName::Literal(tok.literal.into_owned()))
            }
            _ => None,
        }
    }

    /// The mandatory `<name>` in `DROP INDEX`/`DROP CONSTRAINT` (CIP-134
    /// adds the `$param` form; unlike CREATE, DROP always requires a name
    /// token here). Delegates to `parse_optional_schema_name` for the
    /// admissible token set — one spelling of it, so the two readers
    /// cannot drift apart.
    pub(super) fn parse_required_schema_name(&mut self, context: &str) -> ParseResult<SchemaName> {
        match self.parse_optional_schema_name() {
            Some(name) => Ok(name),
            None => {
                let tok = self.peek().clone();
                Err(self.error_at_token(
                    &tok,
                    format!("expected a name after {}, got {:?}", context, tok.literal),
                ))
            }
        }
    }

    /// Consumes an `IF EXISTS` if one comes next; returns whether it did.
    pub(super) fn parse_optional_if_exists(&mut self) -> ParseResult<bool> {
        if self.peek_type() != TokenType::IF {
            return Ok(false);
        }
        self.next_token();
        self.expect(TokenType::EXISTS)?;
        Ok(true)
    }

    /// The `<name>? IF NOT EXISTS? <name>?` prefix both `CREATE INDEX` and
    /// `CREATE CONSTRAINT` accept, in that order: an optional schema name,
    /// an optional `IF NOT EXISTS`, then a second name attempt when the
    /// first one did not parse (so `IF NOT EXISTS` may precede or follow the
    /// name). One spelling of the ordering rule, shared by both parsers.
    pub(super) fn parse_optional_name_and_if_not_exists(
        &mut self,
    ) -> ParseResult<(Option<SchemaName>, bool)> {
        let mut name = self.parse_optional_schema_name();
        let if_not_exists = if self.peek_type() == TokenType::IF {
            self.next_token();
            self.expect(TokenType::NOT)?;
            self.expect(TokenType::EXISTS)?;
            true
        } else {
            false
        };
        if name.is_none() {
            name = self.parse_optional_schema_name();
        }
        Ok((name, if_not_exists))
    }

    /// `<language name> ::= CYPHER <digit>...` -- the version digits are
    /// *not* range-checked here (an unsupported value inside
    /// `DEFAULT LANGUAGE` is a runtime data
    /// exception, not a compile-time syntax error, unlike CIP-150's
    /// statement-preamble `CYPHER <version>` prefix -- the value is
    /// checked where the command executes, the executor's
    /// `catalog_alter_database`, against the one admitted version,
    /// ADR-0584). Only the token shape (`CYPHER` followed by an
    /// integer) is a hard grammar requirement.
    pub(super) fn parse_cypher_language_name(&mut self) -> ParseResult<u32> {
        if !self.peek_is_ident("cypher") {
            let tok = self.peek().clone();
            return Err(self.error_at_token(
                &tok,
                format!("expected CYPHER in <language name>, got {:?}", tok.literal),
            ));
        }
        self.next_token(); // CYPHER
        let tok = self.next_token();
        if tok.ty != TokenType::Integer {
            return Err(self.error_at_token(
                &tok,
                format!(
                    "expected a version number after CYPHER, got {:?}",
                    tok.literal
                ),
            ));
        }
        tok.literal.parse::<u32>().map_err(|_| {
            self.error_at_token(
                &tok,
                format!("invalid cypher version number: {:?}", tok.literal),
            )
        })
    }

    /// `ALTER DATABASE <name> [IF EXISTS] SET DEFAULT LANGUAGE CYPHER
    /// <n>` (CIP-170) -- under ADR-0116 §2's fixed namespaces this is the
    /// *only* supported `ALTER DATABASE` form: a sole `SET DEFAULT
    /// LANGUAGE` item with no `WAIT` clause. Every other form (`SET
    /// ACCESS`, `SET OPTION`, `REMOVE OPTION`, `SET TOPOLOGY`, any
    /// additional `SET` item, or a trailing `WAIT`/`NOWAIT`) is
    /// recognized and rejected with CE's
    /// `UnsupportedAdministrationCommand`.
    pub(super) fn parse_alter_database(&mut self) -> ParseResult<Clause> {
        let start = self.pos;
        self.next_token(); // consume ALTER
        self.next_token(); // consume DATABASE
        let name = self.parse_required_schema_name("ALTER DATABASE")?;
        let if_exists = self.parse_optional_if_exists()?;
        if self.peek_type() == TokenType::SET && self.peek_at_is_ident(1, "default") {
            self.next_token(); // SET
            let default_language = self.parse_alter_database_default_language()?;
            if matches!(self.peek_type(), TokenType::EOF | TokenType::Semicolon) {
                return Ok(Clause::AlterDatabase(AlterDatabaseClause {
                    name,
                    if_exists,
                    default_language,
                }));
            }
            // A second SET item or a WAIT/NOWAIT clause: recognized but
            // unsupported on fixed namespaces (ADR-0116 §2).
            return self.unsupported_admin_clause(start);
        }
        if self.peek_type() == TokenType::SET || self.peek_type() == TokenType::REMOVE {
            return self.unsupported_admin_clause(start);
        }
        let tok = self.peek().clone();
        Err(self.error_at_token(
            &tok,
            format!(
                "expected SET after ALTER DATABASE ... [IF EXISTS], got {:?}",
                tok.literal
            ),
        ))
    }

    /// The one supported `<alter database set item>`: `DEFAULT LANGUAGE
    /// CYPHER <n>` (ADR-0116 §2). The digits are unvalidated at parse time
    /// -- an out-of-range version is a *runtime* data exception (`22N04`).
    pub(super) fn parse_alter_database_default_language(&mut self) -> ParseResult<u32> {
        self.next_token(); // DEFAULT
        self.expect_keyword_ident("LANGUAGE")?;
        self.parse_cypher_language_name()
    }

    /// The one classification of a `CREATE INDEX`'s `ON` target, owning both
    /// `ON EACH` diagnostics: an `EACH` on a kind that does not take it, and
    /// the missing-`EACH` rejection for FULLTEXT and node LOOKUP indexes. A
    /// relationship LOOKUP index is the one shape that may omit `EACH` (live
    /// Neo4j 2026.09; the node form `ON labels(n)` is rejected there with
    /// 42I06, expected "EACH").
    fn classify_on_target(
        &self,
        kind: IndexKind,
        entity_type: SchemaEntityType,
        each_next: bool,
    ) -> ParseResult<OnTargetForm> {
        match (kind, each_next) {
            (IndexKind::Fulltext, true) => Ok(OnTargetForm::EachBracketList),
            (IndexKind::Lookup, true) => Ok(OnTargetForm::LookupTarget),
            (IndexKind::Lookup, false) if entity_type == SchemaEntityType::Relationship => {
                Ok(OnTargetForm::LookupTarget)
            }
            (IndexKind::Fulltext | IndexKind::Lookup, false) => Err(self.error(
                "InvalidSyntax: FULLTEXT/LOOKUP indexes require ON EACH [...] / ON EACH labels(n)/type(r)",
            )),
            (_, true) => Err(self.error_at_token(
                self.peek(),
                "InvalidSyntax: ON EACH is only supported for FULLTEXT and LOOKUP indexes"
                    .to_string(),
            )),
            (_, false) => Ok(OnTargetForm::PropertyList),
        }
    }

    pub(super) fn parse_create_index(&mut self, kind: IndexKind) -> ParseResult<CreateIndexClause> {
        self.next_token(); // consume INDEX

        let (name, if_not_exists) = self.parse_optional_name_and_if_not_exists()?;

        if self.peek_type() == TokenType::ON {
            let (label, properties) = self.parse_legacy_index_target()?;
            return Ok(CreateIndexClause {
                name,
                entity_type: SchemaEntityType::Node,
                labels: vec![label],
                properties,
                if_not_exists,
                kind,
                on_each: false,
                with_properties: Vec::new(),
                options: OptionsForm::Absent,
            });
        }

        let (entity_type, labels, variable) = self.parse_schema_for_target()?;

        self.expect(TokenType::ON)?;

        let each_next = self.peek_is_ident("each");
        let (properties, on_each) = match self.classify_on_target(kind, entity_type, each_next)? {
            OnTargetForm::LookupTarget => {
                if each_next {
                    self.next_token(); // consume EACH
                }
                self.parse_lookup_each_target(entity_type, &variable)?;
                (Vec::new(), each_next)
            }
            OnTargetForm::EachBracketList => {
                self.next_token(); // consume EACH
                (self.parse_on_each_bracket_list()?, true)
            }
            OnTargetForm::PropertyList => (self.parse_on_property_list()?, false),
        };

        // CIP-235: `WITH [var.prop1, var.prop2, ...]` -- a VECTOR-only
        // clause listing additional filtering properties, positioned
        // between `ON` and `OPTIONS`. Reuses the `ON EACH [...]`
        // bracket-list parser, which already rejects an empty list (CF3)
        // and duplicate properties within the list (CF8) with exactly the
        // shape this clause needs.
        let with_properties = if kind == IndexKind::Vector && self.peek_type() == TokenType::WITH {
            self.next_token(); // consume WITH
            self.parse_on_each_bracket_list()?
        } else {
            Vec::new()
        };

        let options = self.parse_options_tail()?;

        Ok(CreateIndexClause {
            name,
            entity_type,
            labels,
            properties,
            if_not_exists,
            kind,
            on_each,
            with_properties,
            options,
        })
    }

    /// The optional `OPTIONS` tail shared by every schema DDL command:
    /// `OPTIONS { ... }` (a literal map), `OPTIONS $param` (resolved
    /// against the query's parameter map at execution time), or no clause
    /// at all.
    pub(super) fn parse_options_tail(&mut self) -> ParseResult<OptionsForm> {
        if self.peek_type() != TokenType::OPTIONS {
            return Ok(OptionsForm::Absent);
        }
        self.next_token();
        if self.peek_type() == TokenType::Parameter {
            let tok = self.next_token();
            return Ok(OptionsForm::Parameter(tok.literal.into_owned()));
        }
        let pairs = self.parse_map_literal()?;
        // Map-bridge: OPTIONS keys are value-domain map keys (evaluated into
        // a runtime `Value::Map`), not scope names — the one MapPair→value
        // feed; SmallName scope maps (ADR-0509 stage 2) do not change it.
        Ok(OptionsForm::Literal(
            pairs
                .into_iter()
                .map(|p| (p.key.to_string(), p.value))
                .collect(),
        ))
    }

    /// Parses the legacy openCypher index target retained by Relate,
    /// `ON :Label(property[, ...])`, starting at `ON`, for `CREATE INDEX`
    /// and `DROP INDEX`. Returns the label (a keyword included, ADR-0169)
    /// and at least one property name; `parse_drop` rejects a legacy
    /// `DROP INDEX` target with more than one.
    pub(super) fn parse_legacy_index_target(&mut self) -> ParseResult<(SmallName, Vec<SmallName>)> {
        self.expect(TokenType::ON)?;
        self.expect(TokenType::Colon)?;
        let label = self.parse_label_name()?;
        self.expect(TokenType::LeftParen)?;
        let mut properties = vec![self.parse_bare_name("property name")?];
        while self.peek_type() == TokenType::Comma {
            self.next_token();
            properties.push(self.parse_bare_name("property name")?);
        }
        self.expect(TokenType::RightParen)?;
        Ok((label, properties))
    }

    /// One `variable.property` term of a constraint's `REQUIRE` schema. The
    /// variable must be the `FOR` clause's own variable: Neo4j rejects any
    /// other name with 42N62 (`Variable `n` not defined`), before the
    /// constraint reaches validation. The detail tag names that condition so
    /// `EngineError::kind()` surfaces `UndefinedVariable` here too, and the
    /// property name goes through the canonical symbolic-name reader
    /// (ADR-0191: an identifier or a keyword, so `n.order` stays legal) so a
    /// non-name token (`n.$p`) is a parse error, not a property literally
    /// named by it.
    fn parse_constraint_property(
        &mut self,
        variable: &SmallName,
        properties: &mut Vec<SmallName>,
    ) -> ParseResult<()> {
        let var_tok = self.next_token();
        // Text compare against the borrowed token (the one place the
        // FOR-pattern variable is matched); the pushed property names are
        // SmallNames from the shared name readers.
        if variable.with_str(|expected| var_tok.literal != expected) {
            return Err(self
                .error_at_token(
                    &var_tok,
                    format!("Variable `{}` not defined", var_tok.literal),
                )
                .with_detail(crate::parser::UNDEFINED_VARIABLE_DETAIL));
        }
        self.expect(TokenType::Dot)?;
        properties.push(self.parse_schema_name("property name")?);
        Ok(())
    }

    pub(super) fn parse_create_constraint(&mut self) -> ParseResult<CreateConstraintClause> {
        self.next_token(); // consume CONSTRAINT

        let (name, if_not_exists) = self.parse_optional_name_and_if_not_exists()?;

        let ConstraintForTarget {
            entity_type,
            mut labels,
            variable,
            source_alias,
            target_alias,
        } = self.parse_constraint_for_target_ext()?;
        // A constraint covers exactly one label or relationship type: an
        // `A|B|...` list is ambiguous and a label-less target names no
        // schema entity at all (never a defaulted empty name).
        if labels.len() > 1 {
            return Err(self.error(
                "constraints support only a single label or relationship type, not an A|B|... list",
            ));
        }
        let Some(label) = labels.pop() else {
            return Err(
                self.error("constraints require a label or relationship type in the FOR target")
            );
        };

        self.expect(TokenType::REQUIRE)?;

        // `REQUIRE <alias>:<Label>` -- a label existence
        // constraint (NLABELEXIST / ESRCLABELEXIST / EDESTLABELEXIST),
        // distinct from every other REQUIRE form (all of which continue
        // with `.` into a property path). Neo4j has no runtime for this
        // (the DDL sketch is explicitly illustrative, not normative),
        // so this is Relate's implementation target derived from the
        // CIP's notation: a node-entity `alias:Label` requires the FOR
        // target's own label to co-occur with `Label`; a relationship
        // entity's `alias` must name one of the pattern's own source/
        // destination endpoint aliases, selecting which endpoint the
        // constraint applies to.
        if self.peek_type().is_ident_like() && self.peek_at(1).ty == TokenType::Colon {
            return self.parse_label_existence_require(
                name,
                if_not_exists,
                label,
                (entity_type, variable, source_alias, target_alias),
            );
        }

        let mut properties = Vec::new();
        if self.peek_type() == TokenType::LeftParen {
            self.next_token();
            loop {
                self.parse_constraint_property(&variable, &mut properties)?;
                if self.peek_type() != TokenType::Comma {
                    break;
                }
                self.next_token();
            }
            self.expect(TokenType::RightParen)?;
        } else {
            self.parse_constraint_property(&variable, &mut properties)?;
        }
        // 22N75: a composite constraint may not repeat a property.
        let mut seen = std::collections::HashSet::new();
        if properties.iter().any(|property| !seen.insert(property)) {
            return Err(self.error("constraint contains duplicated tokens"));
        }

        // CIP-216 01#2/#4: the bare `x :: <value type>` surface form (CIP-86)
        // is equivalent to `x IS :: <value type>` and skips the `IS` keyword
        // entirely -- checked before the mandatory `expect(IS)` below, which
        // covers every other constraint-verb spelling (all of which require
        // `IS`).
        if self.peek_type() == TokenType::DoubleColon {
            self.next_token();
            let te = self.parse_value_type()?;
            let options = self.parse_options_tail()?;
            return Ok(CreateConstraintClause {
                name,
                entity_type,
                label,
                variable,
                properties,
                constraint_type: "propertyType".to_string(),
                if_not_exists,
                property_type: Some(te),
                options,
                enforced_label: None,
            });
        }

        self.expect(TokenType::IS)?;
        let tok = self.peek().clone();
        let (constraint_type, property_type) = match tok.ty {
            TokenType::UNIQUE => {
                self.next_token();
                ("unique".to_string(), None)
            }
            TokenType::NOT => {
                self.next_token();
                self.expect(TokenType::NULL)?;
                ("exists".to_string(), None)
            }
            // CIP-62: the entity keyword (NODE / REL / RELATIONSHIP) before
            // UNIQUE/KEY is optional and purely documentary -- the real
            // entity comes from the FOR-clause target, already captured in
            // `entity_type` above -- so it's just consumed and discarded
            // here. `IS KEY` with no entity keyword at all is also
            // accepted (bare KEY, matched below).
            TokenType::NODE | TokenType::REL => {
                self.next_token(); // consume NODE/REL
                self.parse_constraint_verb()?
            }
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("relationship") => {
                self.next_token(); // consume RELATIONSHIP
                self.parse_constraint_verb()?
            }
            TokenType::Identifier if tok.literal.eq_ignore_ascii_case("key") => {
                self.next_token();
                ("key".to_string(), None)
            }
            TokenType::DoubleColon => {
                // REQUIRE (n.prop) IS :: <value type>  (CIP-85 property
                // type constraint, reusing the CIP-86 <value type> parser).
                self.next_token();
                let te = self.parse_value_type()?;
                ("propertyType".to_string(), Some(te))
            }
            _ if self.peek_is_typed_keyword() => {
                // REQUIRE (n.prop) IS TYPED <value type>
                self.next_token();
                let te = self.parse_value_type()?;
                ("propertyType".to_string(), Some(te))
            }
            _ => {
                return Err(self.error_at_token(
                    &tok,
                    format!(
                        "expected UNIQUE, NOT NULL, KEY, NODE/REL[ATIONSHIP] UNIQUE, \
                         NODE/REL[ATIONSHIP] KEY, ::, or TYPED, got {:?}",
                        tok.literal
                    ),
                ));
            }
        };

        let options = self.parse_options_tail()?;

        Ok(CreateConstraintClause {
            name,
            entity_type,
            label,
            variable,
            properties,
            constraint_type,
            if_not_exists,
            property_type,
            options,
            enforced_label: None,
        })
    }

    /// The `REQUIRE <alias>:<Label>` tail of `CREATE CONSTRAINT` -- a label
    /// existence constraint (NLABELEXIST / ESRCLABELEXIST / EDESTLABELEXIST),
    /// distinct from every other REQUIRE form (all of which continue with
    /// `.` into a property path). Neo4j has no runtime for this (the DDL
    /// sketch is explicitly illustrative, not normative), so this is
    /// Relate's implementation target derived from the CIP's notation: a
    /// node-entity `alias:Label` requires the FOR target's own label to
    /// co-occur with `Label`; a relationship entity's `alias` must name one
    /// of the pattern's own source/destination endpoint aliases, selecting
    /// which endpoint the constraint applies to. `target` is the FOR
    /// target's `(entity, variable, source alias, destination alias)`.
    fn parse_label_existence_require(
        &mut self,
        name: Option<SchemaName>,
        if_not_exists: bool,
        label: SmallName,
        target: (
            SchemaEntityType,
            SmallName,
            Option<SmallName>,
            Option<SmallName>,
        ),
    ) -> ParseResult<CreateConstraintClause> {
        let (entity_type, variable, source_alias, target_alias) = target;
        let alias_tok = self.next_token();
        self.expect(TokenType::Colon)?;
        let target_label = self.parse_label_name()?;
        let constraint_type = match entity_type {
            // Distinct from `"labelExistence"` (only ever created by
            // `ALTER CURRENT GRAPH TYPE`, CIP-187): both render the same
            // `NODE_LABEL_EXISTENCE` SHOW CONSTRAINTS type, but this
            // standalone `CREATE CONSTRAINT` form is an independent
            // constraint, not one the current graph type manages -- see
            // `classification` in `clause::show`.
            SchemaEntityType::Node => "nodeLabelExistence",
            SchemaEntityType::Relationship => {
                let alias = SmallName::from_name(&alias_tok.literal);
                if source_alias.as_ref() == Some(&alias) {
                    "sourceLabelExistence"
                } else if target_alias.as_ref() == Some(&alias) {
                    "targetLabelExistence"
                } else {
                    return Err(self.error_at_token(
                        &alias_tok,
                        format!(
                            "expected the source or destination endpoint alias, got {:?}",
                            alias_tok.literal
                        ),
                    ));
                }
            }
        };
        Ok(CreateConstraintClause {
            name,
            entity_type,
            label,
            variable,
            properties: Vec::new(),
            constraint_type: constraint_type.to_string(),
            if_not_exists,
            property_type: None,
            options: OptionsForm::Absent,
            enforced_label: Some(target_label),
        })
    }

    /// The `UNIQUE`/`KEY` verb following an optional `NODE`/`REL`/
    /// `RELATIONSHIP` entity keyword in `REQUIRE (...) IS [entity] <verb>`
    /// (CIP-62). The entity keyword itself was already consumed by the
    /// caller and discarded -- see the comment at the call site.
    pub(super) fn parse_constraint_verb(&mut self) -> ParseResult<(String, Option<TypeExpr>)> {
        let tok = self.next_token();
        if tok.ty == TokenType::UNIQUE {
            Ok(("unique".to_string(), None))
        } else if tok.literal.eq_ignore_ascii_case("key") {
            Ok(("key".to_string(), None))
        } else {
            Err(self.error_at_token(
                &tok,
                format!("expected UNIQUE or KEY, got {:?}", tok.literal),
            ))
        }
    }

    /// `TERMINATE TRANSACTION[S] <id-list> [YIELD ...]` (CIP-68 grammar
    /// foundation). Not a `SHOW` spelling -- a separate top-level
    /// context-sensitive keyword, same recognition technique.
    pub(super) fn parse_terminate_transactions(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume TERMINATE

        // TRANSACTION has no reserved token of its own, so the
        // identifier-spelling check is the whole gate (same technique as
        // FUNCTION(S)/DEFINED).
        self.expect_kind_word(
            TokenType::Identifier,
            "transaction",
            Some("transactions"),
            "TRANSACTION(S) after TERMINATE",
        )?;

        let ids = self.parse_comma_separated_exprs()?;

        let yields = self.parse_show_yield_list()?;

        Ok(Clause::TerminateTransactions(TerminateTransactionsClause {
            ids,
            yields,
        }))
    }
}

mod graph_type;
